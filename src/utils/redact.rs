//! Secret redaction for outgoing tool output.
//!
//! winx pipes shell output, file contents, and saved memory straight into the
//! model's context - `env`, `cat .env`, CI logs, etc. This scrubs high-confidence
//! credential patterns from any text leaving the server (and from persisted
//! `ContextSave` memory) before it reaches the model or disk.
//!
//! Only **named, low-false-positive** patterns are matched (provider key
//! prefixes, JWTs, PEM private-key blocks, `user:pass@` URLs). There is no
//! entropy heuristic, so ordinary code is never mangled. Set `WINX_NO_REDACT=1`
//! to disable entirely (e.g. when you knowingly need the raw value).
//!
//! A redacted span becomes `[REDACTED:<rule>]` - the rule name only, never any
//! part of the secret.

use std::borrow::Cow;
use std::sync::OnceLock;

use regex::Regex;

/// Named credential patterns, applied in order. Each match is replaced wholesale
/// with `[REDACTED:<name>]`. Kept deliberately specific to avoid false positives.
fn rules() -> &'static [(&'static str, Regex)] {
    static RULES: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    RULES.get_or_init(|| {
        let specs: &[(&str, &str)] = &[
            // PEM private key blocks (any kind). `(?s)` so `.` spans newlines.
            (
                "private-key",
                r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
            ),
            // Upper bounds keep a stray `sk-`/`ghp_` prefix on a long base64 blob
            // from swallowing the whole blob, while staying generous enough for
            // real (incl. long `sk-proj-`) keys.
            ("anthropic-key", r"sk-ant-[A-Za-z0-9_-]{20,200}"),
            ("openai-key", r"sk-(?:proj-)?[A-Za-z0-9_-]{20,200}"),
            ("github-pat", r"github_pat_[A-Za-z0-9_]{22,90}"),
            // Upper bound 255: new-format `ghs_`/`gho_` tokens run well past the
            // 40-char classic length, and an 80-char cap let the tail of a longer
            // token leak past `[REDACTED]` into output and ContextSave memory.
            ("github-token", r"gh[pousr]_[A-Za-z0-9]{36,255}\b"),
            ("slack-token", r"xox[baprs]-[A-Za-z0-9-]{10,}"),
            ("google-api-key", r"AIza[0-9A-Za-z_-]{35}"),
            ("aws-access-key", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
            ("jwt", r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}"),
            // scheme://user:password@host  -> redacts the whole prefix up to '@'.
            ("url-credentials", r"[a-zA-Z][a-zA-Z0-9+.\-]*://[^:@/\s]+:[^@/\s]+@"),
            // First token char must be alnum/_/- so `bearer ./../etc` (a path) is
            // not mistaken for a token; the rest allows the token68 alphabet.
            ("bearer-token", r"(?i)bearer\s+[A-Za-z0-9_-][A-Za-z0-9._~+/-]{15,}={0,2}"),
        ];
        // `.expect`, not `.ok()`: these patterns are compile-time literals. A
        // silently-dropped rule (the old `filter_map(... .ok())`) would let that
        // entire credential class — private keys, JWTs — flow through unredacted
        // with no log or error. Failing loud at first use is the safe default;
        // `redact_rules_compile` below also catches a bad pattern in CI.
        #[allow(clippy::expect_used)]
        specs
            .iter()
            .map(|(name, pat)| {
                let re = Regex::new(pat).expect("redaction rule pattern must compile");
                (*name, re)
            })
            .collect()
    })
}

/// Whether redaction is disabled via `WINX_NO_REDACT`.
fn disabled() -> bool {
    std::env::var("WINX_NO_REDACT").is_ok_and(|v| v != "0" && !v.is_empty())
}

/// Scrub credential patterns from `text`. Returns `Cow::Borrowed` (no allocation)
/// when nothing matched, so the common case is cheap. The `regex` crate is
/// linear-time, so this is safe on adversarial/large input.
pub fn redact(text: &str) -> Cow<'_, str> {
    if disabled() || text.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut owned: Option<String> = None;
    for (name, re) in rules() {
        let current: &str = owned.as_deref().unwrap_or(text);
        // `replace_all` already returns `Cow::Borrowed` when nothing matched, so the
        // separate `is_match` pre-check just doubled the regex passes on every MCP
        // response. Drive off the returned Cow instead — one scan per rule.
        if let Cow::Owned(replaced) =
            re.replace_all(current, |_: &regex::Captures| format!("[REDACTED:{name}]"))
        {
            owned = Some(replaced);
        }
    }
    owned.map_or(Cow::Borrowed(text), Cow::Owned)
}

/// Recursively redact every string inside a JSON value (for `structuredContent`).
pub fn redact_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => {
            if let Cow::Owned(r) = redact(s) {
                *s = r;
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(redact_json),
        serde_json::Value::Object(map) => map.values_mut().for_each(redact_json),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// A known secret isolated by separators is ALWAYS redacted, wrapped in any
        /// surrounding punctuation/whitespace — the raw value never survives into
        /// output and a marker is emitted.
        #[test]
        fn isolated_secret_never_survives(
            before in "[ \t=:;,(]{1,8}",
            after in "[ \t=:;,)\n]{1,8}",
            which in 0usize..2,
        ) {
            let secret = [
                "AKIAIOSFODNN7EXAMPLE",
                "ghp_0123456789012345678901234567890123456",
            ][which];
            let input = format!("{before}{secret}{after}");
            let out = redact(&input);
            prop_assert!(!out.contains(secret), "secret leaked: {out:?}");
            prop_assert!(out.contains("[REDACTED:"), "no marker emitted: {out:?}");
        }

        /// Redaction is idempotent: scrubbing already-scrubbed text changes nothing
        /// (the `[REDACTED:..]` markers must not themselves match any rule).
        #[test]
        fn redaction_is_idempotent(s in "[ -~]{0,80}") {
            let once = redact(&s).into_owned();
            let twice = redact(&once).into_owned();
            prop_assert_eq!(once, twice);
        }
    }

    #[test]
    fn redacts_known_credentials() {
        let cases = [
            "AKIAIOSFODNN7EXAMPLE",
            "ghp_0123456789012345678901234567890123456",
            "sk-ant-api03-abcdefghijklmnopqrstuvwxyz",
            "AIzaSyA1234567890123456789012345678901234",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abcDEF123456",
            "postgres://user:hunter2@db.local/app",
        ];
        for c in cases {
            let red = redact(c);
            assert!(red.contains("[REDACTED:"), "did not redact: {c} -> {red}");
            assert!(!red.contains("hunter2"));
        }
    }

    #[test]
    fn redacts_pem_private_key_block() {
        let pem = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIBOgIBAAJBAKj34Gkx...\n-----END RSA PRIVATE KEY-----\nafter";
        let red = redact(pem);
        assert!(red.contains("before") && red.contains("after"));
        assert!(red.contains("[REDACTED:private-key]"));
        assert!(!red.contains("MIIBOgIBAAJBAKj"));
    }

    #[test]
    fn leaves_ordinary_code_untouched() {
        let code = "pub fn parse_config(path: &str) -> Result<Config> { Ok(Config::default()) }";
        assert!(matches!(redact(code), Cow::Borrowed(_)));
    }

    #[test]
    fn bearer_does_not_eat_a_path() {
        // `bearer ./../etc/passwd` is a path, not a token - must not be redacted.
        let line = "run: bearer ./../etc/passwd && cat file";
        assert!(matches!(redact(line), Cow::Borrowed(_)), "redacted a path as a bearer token");
        // a real bearer token still redacts
        let auth = "Authorization: Bearer abcdef0123456789ABCDEF";
        assert!(redact(auth).contains("[REDACTED:bearer-token]"));
    }

    #[test]
    fn redacts_inside_surrounding_text() {
        let line = "export GITHUB_TOKEN=ghp_0123456789012345678901234567890123456 # ci";
        let red = redact(line);
        assert!(red.contains("export GITHUB_TOKEN="));
        assert!(red.contains("[REDACTED:github-token]"));
        assert!(red.contains("# ci"));
    }

    #[test]
    fn redact_rules_compile() {
        // Forces `rules()` to build every pattern. If any literal regresses into an
        // invalid pattern, this fails in CI instead of panicking in production on
        // the first redact() call (or, pre-fix, silently dropping the rule).
        let names: Vec<&str> = rules().iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"private-key"));
        assert!(names.contains(&"github-token"));
        assert_eq!(names.len(), 11, "rule count changed — update this assertion");
    }

    #[test]
    fn redacts_long_github_token() {
        // A 120-char ghs_ token must be redacted whole — no tail leaking past the
        // marker (the old {36,80} bound truncated the match).
        let tok = format!("ghs_{}", "a".repeat(120));
        let line = format!("token={tok} done");
        let red = redact(&line);
        assert!(red.contains("[REDACTED:github-token]"), "not redacted: {red}");
        assert!(!red.contains("aaaaaaaaaa"), "token tail leaked: {red}");
    }

    #[test]
    fn redacts_json_strings() {
        let mut v = serde_json::json!({"out": "key=AKIAIOSFODNN7EXAMPLE", "n": 5, "ok": true});
        redact_json(&mut v);
        assert!(v["out"].as_str().unwrap().contains("[REDACTED:"));
        assert_eq!(v["n"], 5);
    }
}
