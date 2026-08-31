//! Data-driven turn detection: TOML rule manifests evaluated against the
//! rendered screen plus OSC title/progress strings.
//!
//! This is a port of herdr's agent-detection engine
//! (<https://github.com/herdrdev/herdr>, `src/detect/manifest.rs`, Apache-2.0;
//! vendored at commit `0cbd1a5aa847ab767334938e3bc858c68e613d70`), trimmed to
//! what winx needs: the bundled manifests in `agent_manifests/` (see the
//! attribution README there), an optional local override directory, and the
//! rule/region/gate evaluation semantics — no remote manifest updates.
//!
//! One deliberate deviation from herdr: when no rule matches, herdr assumes a
//! known agent is *idle*; winx returns [`TurnState::Unknown`] instead so the
//! caller's quiescence check (and any legacy recognizer fallback) stays the
//! authority. herdr can afford the idle fallback because PTY activity is its
//! working-state authority; in winx that role belongs to quiescence.
//!
//! Rule semantics (kept bit-compatible with herdr so manifests drop in
//! unmodified):
//!   * every `contains` needle (lowercased), `regex`, and `line_regex` in a
//!     gate must match (AND); `line_regex` needs some line to match per
//!     pattern; `any` needs at least one nested gate; `not` fails the gate if
//!     any nested gate matches;
//!   * rules are evaluated in file order and the highest-priority match wins
//!     (first match kept on ties);
//!   * a rule's `region` selects what text it sees: the whole screen, bottom /
//!     top line windows, prompt-marker slices, the prompt box, or the OSC
//!     title/progress strings.

use std::collections::HashMap;
use std::sync::{Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use regex::Regex;
use serde::Deserialize;

use crate::state::turn::TurnState;

/// The manifest engine feature level this port implements — mirrors herdr's
/// `MANIFEST_ENGINE_VERSION` so bundled manifests declaring
/// `min_engine_version` up to this value are accepted.
pub const ENGINE_VERSION: u32 = 3;

/// What the detector reads: the rendered, ANSI-stripped screen joined with
/// `\n`, plus the latest OSC 0/2 title and OSC 9 progress payload captured
/// from the raw PTY stream (empty strings when unavailable — behavior then
/// matches a screen-only engine).
#[derive(Debug, Clone, Copy)]
pub struct DetectionInput<'a> {
    pub screen: &'a str,
    pub osc_title: &'a str,
    pub osc_progress: &'a str,
}

/// Outcome of evaluating one manifest: the mapped turn state and, when a rule
/// fired, which one (for the status footer / tracing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    pub state: TurnState,
    /// `Some((rule_id, priority))` when a rule matched; `None` means no rule
    /// fired and `state` is [`TurnState::Unknown`].
    pub rule: Option<(&'static str, i32)>,
}

// --- manifest schema (TOML) --------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    id: String,
    #[allow(dead_code)]
    version: Option<String>,
    min_engine_version: Option<u32>,
    #[serde(rename = "updated_at")]
    _updated_at: Option<String>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    rules: Vec<RawRule>,
}

// The four bools mirror herdr's manifest schema field-for-field so upstream
// TOMLs drop in unmodified — collapsing them would break deserialization.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    id: String,
    state: Option<RawState>,
    #[serde(default)]
    priority: i32,
    #[serde(default = "default_region")]
    region: String,
    // Confidence metadata in herdr's arbitration; winx only needs the state,
    // but the fields must parse so upstream manifests drop in unmodified.
    #[serde(default)]
    #[allow(dead_code)]
    visible_idle: bool,
    #[serde(default)]
    #[allow(dead_code)]
    visible_blocker: bool,
    #[serde(default)]
    #[allow(dead_code)]
    visible_working: bool,
    #[serde(default)]
    skip_state_update: bool,
    #[serde(default)]
    all: Vec<RawGate>,
    #[serde(default)]
    any: Vec<RawGate>,
    #[serde(default, rename = "not")]
    not_gate: Vec<RawGate>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct RawGate {
    #[serde(default)]
    all: Vec<RawGate>,
    #[serde(default)]
    any: Vec<RawGate>,
    #[serde(default, rename = "not")]
    not_gate: Vec<RawGate>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RawState {
    Idle,
    Working,
    Blocked,
    Unknown,
}

impl From<RawState> for TurnState {
    fn from(value: RawState) -> Self {
        match value {
            RawState::Idle => TurnState::AwaitingInput,
            RawState::Working => TurnState::Busy,
            RawState::Blocked => TurnState::AwaitingApproval,
            RawState::Unknown => TurnState::Unknown,
        }
    }
}

fn default_region() -> String {
    "whole_recent".to_string()
}

// --- compiled form -----------------------------------------------------------

/// A parsed, validated, regex-compiled manifest ready for evaluation.
#[derive(Debug)]
pub struct CompiledManifest {
    id: String,
    aliases: Vec<String>,
    rules: Vec<CompiledRule>,
}

#[derive(Debug)]
struct CompiledRule {
    id: String,
    priority: i32,
    region: String,
    state: TurnState,
    gate: CompiledGate,
}

#[derive(Debug)]
struct CompiledGate {
    all: Vec<CompiledGate>,
    any: Vec<CompiledGate>,
    not_gate: Vec<CompiledGate>,
    /// Lowercased at compile time; matched against the lowercased region text.
    contains: Vec<String>,
    regex: Vec<Regex>,
    line_regex: Vec<Regex>,
}

impl CompiledManifest {
    /// The manifest's canonical id (`claude`, `codex`, `gemini`, ...).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Evaluate every rule against `input`; the highest-priority match wins
    /// (first match kept on ties, matching herdr). No match → `Unknown`.
    pub fn detect(&'static self, input: DetectionInput<'_>) -> Verdict {
        let mut matched: Option<&'static CompiledRule> = None;
        for rule in &self.rules {
            let text = region(input, &rule.region);
            if !gate_matches(&rule.gate, text, &text.to_lowercase()) {
                continue;
            }
            match matched {
                Some(previous) if previous.priority >= rule.priority => {}
                _ => matched = Some(rule),
            }
        }
        match matched {
            Some(rule) => {
                Verdict { state: rule.state, rule: Some((rule.id.as_str(), rule.priority)) }
            }
            None => Verdict { state: TurnState::Unknown, rule: None },
        }
    }
}

// --- bundled manifests + lookup ----------------------------------------------

/// Every manifest shipped in the binary. Vendored from herdr — see
/// `agent_manifests/README.md` for provenance and license.
const BUNDLED_MANIFESTS: &[&str] = &[
    include_str!("agent_manifests/amp.toml"),
    include_str!("agent_manifests/antigravity.toml"),
    include_str!("agent_manifests/claude.toml"),
    include_str!("agent_manifests/cline.toml"),
    include_str!("agent_manifests/codex.toml"),
    include_str!("agent_manifests/cursor.toml"),
    include_str!("agent_manifests/devin.toml"),
    include_str!("agent_manifests/droid.toml"),
    include_str!("agent_manifests/gemini.toml"),
    include_str!("agent_manifests/github-copilot.toml"),
    include_str!("agent_manifests/grok.toml"),
    include_str!("agent_manifests/hermes.toml"),
    include_str!("agent_manifests/kilo.toml"),
    include_str!("agent_manifests/kimi.toml"),
    include_str!("agent_manifests/kiro.toml"),
    include_str!("agent_manifests/maki.toml"),
    include_str!("agent_manifests/muse.toml"),
    include_str!("agent_manifests/opencode.toml"),
    include_str!("agent_manifests/pi.toml"),
    include_str!("agent_manifests/qodercli.toml"),
    include_str!("agent_manifests/qwen.toml"),
];

static MANIFESTS: OnceLock<Vec<CompiledManifest>> = OnceLock::new();

/// All compiled bundled manifests, loading (once) on first use. A bundled
/// manifest that fails to compile is logged and skipped rather than panicking —
/// the caller then behaves as if that agent had no manifest (legacy
/// recognizers / quiescence take over); the test suite asserts none actually
/// fail. Local overrides are layered on top by [`manifest_for`], not here.
pub fn manifests() -> &'static [CompiledManifest] {
    MANIFESTS.get_or_init(|| {
        BUNDLED_MANIFESTS
            .iter()
            .filter_map(|content| match compile_manifest(content) {
                Ok(manifest) => Some(manifest),
                Err(err) => {
                    tracing::error!(error = %err, "skipping invalid bundled agent manifest");
                    None
                }
            })
            .collect()
    })
}

/// Resolve a recognizer hint to a manifest by id or alias (case-insensitive):
/// `claude`, `claude-code`, `codex`, `gemini`, `agy`, `cursor`, `opencode`,
/// `copilot`, `grok`, ... When `WINX_AGENT_MANIFEST_DIR` holds a valid
/// `<id>.toml` override it shadows the bundled manifest, and edits to that
/// file are picked up while the process runs (see [`overridden_manifest`]) —
/// a served long-lived winx can hot-fix detection without a restart.
pub fn manifest_for(hint: &str) -> Option<&'static CompiledManifest> {
    static LOOKUP: OnceLock<HashMap<String, usize>> = OnceLock::new();
    let manifests = manifests();
    let lookup = LOOKUP.get_or_init(|| {
        let mut map = HashMap::new();
        for (index, manifest) in manifests.iter().enumerate() {
            map.entry(manifest.id.to_lowercase()).or_insert(index);
            for alias in &manifest.aliases {
                map.entry(alias.to_lowercase()).or_insert(index);
            }
        }
        map
    });
    let bundled =
        lookup.get(&hint.trim().to_lowercase()).and_then(|&index| manifests.get(index))?;
    Some(overridden_manifest(&bundled.id).unwrap_or(bundled))
}

// --- hot-reloadable overrides -------------------------------------------------

/// How long a cached override verdict stays fresh before the file's mtime is
/// checked again. Bounds the stat cost on the `wait_for_turn` poll cadence
/// while keeping manifest hot-fixes near-immediate.
const OVERRIDE_RECHECK_INTERVAL: Duration = Duration::from_secs(2);

/// Cached override state for one manifest id.
struct OverrideSlot {
    last_checked: Option<Instant>,
    /// mtime of the last override file this slot reconciled against (`None` =
    /// file absent). A failed compile still records the mtime — with the
    /// previously compiled override kept — so a later fix (newer mtime)
    /// recompiles.
    mtime: Option<SystemTime>,
    /// Compiled overrides are leaked to `'static` (the manifest API hands out
    /// `&'static` references). Reloads are rare, operator-driven events, so
    /// the leak is bounded by how often a human edits the file.
    manifest: Option<&'static CompiledManifest>,
}

fn override_dir() -> Option<&'static std::path::Path> {
    static DIR: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| std::env::var("WINX_AGENT_MANIFEST_DIR").ok().map(Into::into)).as_deref()
}

/// The active override for `id`, reconciling with disk when its recheck window
/// elapsed. `None` means the bundled manifest is authoritative.
fn overridden_manifest(id: &str) -> Option<&'static CompiledManifest> {
    static SLOTS: OnceLock<StdMutex<HashMap<String, OverrideSlot>>> = OnceLock::new();
    let dir = override_dir()?;
    let mut slots = SLOTS
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let slot = slots.entry(id.to_string()).or_insert(OverrideSlot {
        last_checked: None,
        mtime: None,
        manifest: None,
    });
    if slot.last_checked.is_none_or(|at| at.elapsed() >= OVERRIDE_RECHECK_INTERVAL) {
        refresh_override_slot(slot, &dir.join(format!("{id}.toml")), id);
        slot.last_checked = Some(Instant::now());
    }
    slot.manifest
}

/// Reconcile one override slot with the file at `path`: absent → revert to
/// bundled; unchanged mtime → keep; changed → recompile. A compile error or
/// id mismatch keeps the previously compiled override rather than reverting
/// detection to a state the operator already moved past.
fn refresh_override_slot(slot: &mut OverrideSlot, path: &std::path::Path, id: &str) {
    let Ok(mtime) = std::fs::metadata(path).and_then(|metadata| metadata.modified()) else {
        if slot.manifest.is_some() {
            tracing::info!(id, path = %path.display(), "agent manifest override removed; bundled manifest active again");
        }
        slot.mtime = None;
        slot.manifest = None;
        return;
    };
    if Some(mtime) == slot.mtime {
        return;
    }
    slot.mtime = Some(mtime);
    let compiled = std::fs::read_to_string(path)
        .map_err(|err| err.to_string())
        .and_then(|content| compile_manifest(&content));
    match compiled {
        Ok(manifest) if manifest.id == id => {
            tracing::info!(id, path = %path.display(), "loaded agent manifest override");
            slot.manifest = Some(Box::leak(Box::new(manifest)));
        }
        Ok(manifest) => {
            tracing::warn!(
                id, override_id = %manifest.id, path = %path.display(),
                "ignoring agent manifest override: id mismatch"
            );
        }
        Err(err) => {
            tracing::warn!(
                id, path = %path.display(), error = %err,
                "ignoring agent manifest override: failed to compile"
            );
        }
    }
}

// --- parsing + validation ----------------------------------------------------

// Complexity caps ported from herdr: they bound override-supplied manifests so
// a pathological file can't wedge the detector.
const MAX_RULES_PER_MANIFEST: usize = 128;
const MAX_GATE_DEPTH: usize = 8;
const MAX_TOTAL_GATES: usize = 512;
const MAX_MATCHERS_PER_GATE: usize = 32;
const MAX_TOTAL_MATCHERS: usize = 1024;
const MAX_MATCHER_CHARS: usize = 512;

fn compile_manifest(content: &str) -> Result<CompiledManifest, String> {
    let raw = toml::from_str::<RawManifest>(content).map_err(|err| err.to_string())?;
    validate_manifest(&raw)?;
    let rules = raw
        .rules
        .iter()
        .map(|rule| {
            compile_gate(&gate_from_rule(rule))
                .map(|gate| CompiledRule {
                    id: rule.id.clone(),
                    priority: rule.priority,
                    region: rule.region.trim().to_string(),
                    state: rule.state.map_or(TurnState::Unknown, TurnState::from),
                    gate,
                })
                .map_err(|err| format!("rule {} could not be compiled: {err}", rule.id))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CompiledManifest { id: raw.id, aliases: raw.aliases, rules })
}

fn validate_manifest(manifest: &RawManifest) -> Result<(), String> {
    if manifest.id.trim().is_empty() {
        return Err("manifest id must not be empty".to_string());
    }
    if let Some(version) = manifest.min_engine_version {
        if version > ENGINE_VERSION {
            return Err(format!(
                "manifest requires engine {version}, this engine is {ENGINE_VERSION}"
            ));
        }
    }
    if manifest.rules.is_empty() {
        return Err("manifest must contain at least one rule".to_string());
    }
    if manifest.rules.len() > MAX_RULES_PER_MANIFEST {
        return Err(format!(
            "manifest contains {} rules, max is {MAX_RULES_PER_MANIFEST}",
            manifest.rules.len()
        ));
    }

    let mut complexity = Complexity::default();
    for rule in &manifest.rules {
        if rule.id.trim().is_empty() {
            return Err("manifest rule id must not be empty".to_string());
        }
        // herdr couples skip_state_update to state = "unknown"; the coupling is
        // what lets winx treat those rules as plain Unknown (defer to
        // quiescence) with no extra plumbing.
        if rule.skip_state_update && rule.state != Some(RawState::Unknown) {
            return Err(format!(
                "rule {} uses skip_state_update without state = \"unknown\"",
                rule.id
            ));
        }
        validate_region_name(&rule.region)
            .map_err(|err| format!("rule {} uses invalid region: {err}", rule.id))?;
        validate_gate(&gate_from_rule(rule), "rule", 0, &mut complexity)
            .map_err(|err| format!("rule {} has invalid matcher gates: {err}", rule.id))?;
    }
    Ok(())
}

#[derive(Default)]
struct Complexity {
    total_gates: usize,
    total_matchers: usize,
}

fn validate_gate(
    gate: &RawGate,
    context: &str,
    depth: usize,
    complexity: &mut Complexity,
) -> Result<(), String> {
    if depth > MAX_GATE_DEPTH {
        return Err(format!("{context} exceeds max gate depth {MAX_GATE_DEPTH}"));
    }
    complexity.total_gates += 1;
    if complexity.total_gates > MAX_TOTAL_GATES {
        return Err(format!("manifest exceeds max gate count {MAX_TOTAL_GATES}"));
    }
    validate_matcher_limits(gate, context, complexity)?;
    if !gate_has_positive_matcher(gate) {
        return Err(format!("{context} must contain a positive matcher"));
    }
    validate_regex_patterns(&gate.regex, context, "regex")?;
    validate_regex_patterns(&gate.line_regex, context, "line_regex")?;
    for nested in &gate.all {
        validate_gate(nested, "all gate", depth + 1, complexity)?;
    }
    for nested in &gate.any {
        validate_gate(nested, "any gate", depth + 1, complexity)?;
    }
    for nested in &gate.not_gate {
        validate_not_gate(nested, depth + 1, complexity)?;
    }
    Ok(())
}

fn validate_not_gate(
    gate: &RawGate,
    depth: usize,
    complexity: &mut Complexity,
) -> Result<(), String> {
    if depth > MAX_GATE_DEPTH {
        return Err(format!("not gate exceeds max gate depth {MAX_GATE_DEPTH}"));
    }
    complexity.total_gates += 1;
    if complexity.total_gates > MAX_TOTAL_GATES {
        return Err(format!("manifest exceeds max gate count {MAX_TOTAL_GATES}"));
    }
    validate_matcher_limits(gate, "not gate", complexity)?;
    if !gate_has_any_matcher(gate) {
        return Err("not gate must contain a matcher".to_string());
    }
    validate_regex_patterns(&gate.regex, "not gate", "regex")?;
    validate_regex_patterns(&gate.line_regex, "not gate", "line_regex")?;
    for nested in &gate.all {
        validate_gate(nested, "not all gate", depth + 1, complexity)?;
    }
    for nested in &gate.any {
        validate_gate(nested, "not any gate", depth + 1, complexity)?;
    }
    for nested in &gate.not_gate {
        validate_not_gate(nested, depth + 1, complexity)?;
    }
    Ok(())
}

fn validate_matcher_limits(
    gate: &RawGate,
    context: &str,
    complexity: &mut Complexity,
) -> Result<(), String> {
    let matcher_count = gate.contains.len() + gate.regex.len() + gate.line_regex.len();
    if matcher_count > MAX_MATCHERS_PER_GATE {
        return Err(format!(
            "{context} has {matcher_count} direct matchers, max is {MAX_MATCHERS_PER_GATE}"
        ));
    }
    complexity.total_matchers += matcher_count;
    if complexity.total_matchers > MAX_TOTAL_MATCHERS {
        return Err(format!("manifest exceeds max matcher count {MAX_TOTAL_MATCHERS}"));
    }
    for value in gate.contains.iter().chain(gate.regex.iter()).chain(gate.line_regex.iter()) {
        if value.chars().count() > MAX_MATCHER_CHARS {
            return Err(format!("{context} matcher exceeds max length {MAX_MATCHER_CHARS}"));
        }
    }
    Ok(())
}

fn validate_regex_patterns(patterns: &[String], context: &str, field: &str) -> Result<(), String> {
    for pattern in patterns {
        Regex::new(pattern).map_err(|err| {
            format!("{context} contains invalid {field} pattern {pattern:?}: {err}")
        })?;
    }
    Ok(())
}

fn gate_has_positive_matcher(gate: &RawGate) -> bool {
    !gate.contains.is_empty()
        || !gate.regex.is_empty()
        || !gate.line_regex.is_empty()
        || !gate.all.is_empty()
        || !gate.any.is_empty()
}

fn gate_has_any_matcher(gate: &RawGate) -> bool {
    gate_has_positive_matcher(gate) || !gate.not_gate.is_empty()
}

fn validate_region_name(spec: &str) -> Result<(), String> {
    let trimmed = spec.trim();
    match trimmed {
        "whole_recent"
        | "after_last_prompt_marker"
        | "before_current_prompt_marker"
        | "whole_recent_without_current_prompt_marker"
        | "current_prompt_block_marker"
        | "after_current_prompt_block_marker"
        | "prompt_box_body"
        | "above_prompt_box"
        | "last_non_empty_above_prompt_box"
        | "after_last_horizontal_rule"
        | "osc_title"
        | "osc_progress" => Ok(()),
        _ if region_count(trimmed, "bottom_lines").is_some()
            || region_count(trimmed, "bottom_non_empty_lines").is_some()
            || top_region_count(trimmed).is_some() =>
        {
            Ok(())
        }
        _ => Err(trimmed.to_string()),
    }
}

fn gate_from_rule(rule: &RawRule) -> RawGate {
    RawGate {
        all: rule.all.clone(),
        any: rule.any.clone(),
        not_gate: rule.not_gate.clone(),
        contains: rule.contains.clone(),
        regex: rule.regex.clone(),
        line_regex: rule.line_regex.clone(),
    }
}

fn compile_gate(gate: &RawGate) -> Result<CompiledGate, String> {
    Ok(CompiledGate {
        all: gate.all.iter().map(compile_gate).collect::<Result<_, _>>()?,
        any: gate.any.iter().map(compile_gate).collect::<Result<_, _>>()?,
        not_gate: gate.not_gate.iter().map(compile_gate).collect::<Result<_, _>>()?,
        contains: gate.contains.iter().map(|needle| needle.to_lowercase()).collect(),
        regex: gate
            .regex
            .iter()
            .map(|pattern| Regex::new(pattern).map_err(|err| err.to_string()))
            .collect::<Result<_, _>>()?,
        line_regex: gate
            .line_regex
            .iter()
            .map(|pattern| Regex::new(pattern).map_err(|err| err.to_string()))
            .collect::<Result<_, _>>()?,
    })
}

// --- evaluation --------------------------------------------------------------

fn gate_matches(gate: &CompiledGate, text: &str, lower_text: &str) -> bool {
    if !gate.contains.iter().all(|needle| lower_text.contains(needle)) {
        return false;
    }
    if !gate.regex.iter().all(|regex| regex.is_match(text)) {
        return false;
    }
    if !gate.line_regex.iter().all(|regex| text.lines().any(|line| regex.is_match(line))) {
        return false;
    }
    if !gate.all.iter().all(|nested| gate_matches(nested, text, lower_text)) {
        return false;
    }
    if !gate.any.is_empty() && !gate.any.iter().any(|nested| gate_matches(nested, text, lower_text))
    {
        return false;
    }
    if gate.not_gate.iter().any(|nested| gate_matches(nested, text, lower_text)) {
        return false;
    }
    true
}

// --- regions -----------------------------------------------------------------

fn region<'a>(input: DetectionInput<'a>, spec: &str) -> &'a str {
    // OSC regions source from their dedicated fields, not the screen.
    match spec {
        "osc_title" => return input.osc_title,
        "osc_progress" => return input.osc_progress,
        _ => {}
    }
    let content = input.screen;
    match spec {
        "whole_recent" => content,
        "after_last_prompt_marker" => after_last_prompt_marker(content),
        "before_current_prompt_marker" => before_current_prompt_marker(content),
        "whole_recent_without_current_prompt_marker" => {
            whole_recent_without_current_prompt_marker(content)
        }
        "current_prompt_block_marker" => current_prompt_block_marker(content).unwrap_or(""),
        "after_current_prompt_block_marker" => {
            after_current_prompt_block_marker(content).unwrap_or("")
        }
        "prompt_box_body" => prompt_box_body(content).unwrap_or(""),
        "above_prompt_box" => above_prompt_box(content),
        "last_non_empty_above_prompt_box" => last_non_empty_line(above_prompt_box(content)),
        "after_last_horizontal_rule" => after_last_horizontal_rule(content),
        _ => {
            if let Some(count) = region_count(spec, "bottom_lines") {
                return bottom_lines(content, count);
            }
            if let Some(count) = region_count(spec, "bottom_non_empty_lines") {
                return bottom_non_empty_lines(content, count);
            }
            if let Some(count) = top_region_count(spec) {
                return top_non_empty_lines(content, count);
            }
            ""
        }
    }
}

fn region_count(spec: &str, name: &str) -> Option<usize> {
    spec.strip_prefix(name)
        .and_then(|rest| rest.strip_prefix('('))
        .and_then(|rest| rest.strip_suffix(')'))
        .and_then(|count| count.parse::<usize>().ok())
}

const MAX_TOP_REGION_LINE_COUNT: usize = u16::MAX as usize;

fn top_region_count(spec: &str) -> Option<usize> {
    let count = spec.strip_prefix("top_non_empty_lines")?.strip_prefix('(')?.strip_suffix(')')?;
    if count.starts_with('0') || !count.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    count.parse::<usize>().ok().filter(|count| *count <= MAX_TOP_REGION_LINE_COUNT)
}

fn bottom_lines(content: &str, count: usize) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(count);
    slice_from_line_index(content, &lines, start)
}

fn bottom_non_empty_lines(content: &str, count: usize) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(start_index) = lines
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, line)| !line.trim().is_empty())
        .take(count)
        .last()
        .map(|(index, _)| index)
    else {
        return "";
    };
    slice_from_line_index(content, &lines, start_index)
}

fn top_non_empty_lines(content: &str, count: usize) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(end_index) = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .take(count)
        .last()
        .map(|(index, _)| index)
    else {
        return "";
    };
    let byte_offset = line_start_offset(content, &lines, end_index + 1);
    &content[..byte_offset]
}

fn after_last_prompt_marker(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(index) = lines.iter().rposition(|line| codex_prompt_line(line)) else {
        return content;
    };
    slice_from_line_index(content, &lines, index + 1)
}

fn before_current_prompt_marker(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(index) = current_codex_prompt_index(&lines) else {
        return content;
    };
    let byte_offset = lines[..index].iter().map(|line| line.len() + 1).sum::<usize>();
    &content[..byte_offset.min(content.len())]
}

fn whole_recent_without_current_prompt_marker(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    if current_codex_prompt_index(&lines).is_some() {
        ""
    } else {
        content
    }
}

fn current_prompt_block_marker(content: &str) -> Option<&str> {
    let lines: Vec<&str> = content.lines().collect();
    let prompt_index = current_codex_prompt_index(&lines)?;
    lines[..prompt_index].iter().rev().find(|line| codex_block_marker_line(line)).copied()
}

fn after_current_prompt_block_marker(content: &str) -> Option<&str> {
    let lines: Vec<&str> = content.lines().collect();
    let prompt_index = current_codex_prompt_index(&lines)?;
    let block_index =
        lines[..prompt_index].iter().rposition(|line| codex_block_marker_line(line))?;
    Some(slice_from_line_index(content, &lines, block_index))
}

fn current_codex_prompt_index(lines: &[&str]) -> Option<usize> {
    let prompt_index = lines.iter().rposition(|line| codex_prompt_line(line))?;
    if lines[prompt_index + 1..].iter().any(|line| codex_block_marker_line(line)) {
        return None;
    }
    Some(prompt_index)
}

fn codex_prompt_line(line: &str) -> bool {
    line == "›" || line.starts_with("› ")
}

fn codex_block_marker_line(line: &str) -> bool {
    line.starts_with('•') || line.starts_with('■') || line.starts_with('✗') || line.starts_with('✓')
}

fn prompt_box_body(content: &str) -> Option<&str> {
    let lines: Vec<&str> = content.lines().collect();
    let top = prompt_box_top_border_index(&lines)?;
    let start = line_start_offset(content, &lines, top + 1);
    let end_index = lines[top + 1..]
        .iter()
        .position(|line| is_horizontal_rule(line))
        .map_or(lines.len(), |relative| top + 1 + relative);
    let end = line_start_offset(content, &lines, end_index);
    Some(&content[start.min(content.len())..end.min(content.len())])
}

fn above_prompt_box(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(top) = prompt_box_top_border_index(&lines) else {
        return content;
    };
    let end = line_start_offset(content, &lines, top);
    &content[..end.min(content.len())]
}

fn after_last_horizontal_rule(content: &str) -> &str {
    let mut last_rule_end = 0usize;
    let mut offset = 0usize;
    for line in content.lines() {
        let next_offset = offset + line.len() + 1;
        if is_horizontal_rule(line) {
            last_rule_end = next_offset.min(content.len());
        }
        offset = next_offset;
    }
    &content[last_rule_end..]
}

fn last_non_empty_line(content: &str) -> &str {
    content.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("")
}

fn prompt_box_top_border_index(lines: &[&str]) -> Option<usize> {
    let mut border_count = 0;
    for index in (0..lines.len()).rev() {
        if is_horizontal_rule(lines[index]) {
            border_count += 1;
            if border_count == 2 {
                return Some(index);
            }
        }
    }
    None
}

fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    let rule_chars = trimmed.chars().take_while(|&ch| ch == '─').count();
    if rule_chars == 0 {
        return false;
    }
    let rule_bytes =
        trimmed.char_indices().nth(rule_chars).map_or(trimmed.len(), |(index, _)| index);
    let suffix = trimmed[rule_bytes..].trim_start();
    suffix.is_empty() || rule_chars >= 3
}

fn slice_from_line_index<'a>(content: &'a str, lines: &[&str], index: usize) -> &'a str {
    let byte_offset = line_start_offset(content, lines, index);
    &content[byte_offset.min(content.len())..]
}

fn line_start_offset(content: &str, lines: &[&str], index: usize) -> usize {
    lines[..index.min(lines.len())]
        .iter()
        .map(|line| line.len() + 1)
        .sum::<usize>()
        .min(content.len())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn input(screen: &str) -> DetectionInput<'_> {
        DetectionInput { screen, osc_title: "", osc_progress: "" }
    }

    #[test]
    fn every_bundled_manifest_compiles() {
        // Runtime skips invalid bundled manifests instead of panicking; this
        // is the gate that keeps that path dead.
        for content in BUNDLED_MANIFESTS {
            if let Err(err) = compile_manifest(content) {
                let id = content.lines().next().unwrap_or("").to_string();
                panic!("bundled manifest failed to compile ({id}): {err}");
            }
        }
        assert_eq!(manifests().len(), BUNDLED_MANIFESTS.len());
    }

    #[test]
    fn lookup_resolves_ids_and_aliases() {
        assert_eq!(manifest_for("claude").map(CompiledManifest::id), Some("claude"));
        assert_eq!(manifest_for("claude-code").map(CompiledManifest::id), Some("claude"));
        assert_eq!(manifest_for("CODEX").map(CompiledManifest::id), Some("codex"));
        assert_eq!(manifest_for("antigravity").map(CompiledManifest::id), Some("agy"));
        assert_eq!(manifest_for("gemini").map(CompiledManifest::id), Some("gemini"));
        assert_eq!(manifest_for("ghcs").map(CompiledManifest::id), Some("copilot"));
        assert_eq!(manifest_for("cursor-agent").map(CompiledManifest::id), Some("cursor"));
        assert!(manifest_for("not-an-agent").is_none());
    }

    #[test]
    fn claude_busy_via_footer_and_idle_prompt_box() {
        let claude = manifest_for("claude").expect("claude manifest");
        // Busy: the live-turn footer (herdr's live_turn_working rule).
        let busy = "❯ prompt\n\n* Hatching…\n⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt";
        let verdict = claude.detect(input(busy));
        assert_eq!(verdict.state, TurnState::Busy);
        assert_eq!(verdict.rule.map(|(id, _)| id), Some("live_turn_working"));

        // Idle: the ❯ prompt box between two horizontal rules.
        let idle = "●─PONG\n──────────\n❯ \n──────────\n  ⏵⏵ bypass permissions on";
        let verdict = claude.detect(input(idle));
        assert_eq!(verdict.state, TurnState::AwaitingInput);
        assert_eq!(verdict.rule.map(|(id, _)| id), Some("live_prompt_box"));
    }

    #[test]
    fn claude_tool_permission_dialog_is_approval() {
        let claude = manifest_for("claude").expect("claude manifest");
        let screen = "⏺ Bash(rm -rf /tmp/x)\nDo you want to proceed?\n❯ 1. Yes\n  2. No, and tell Claude what to do differently";
        assert_eq!(claude.detect(input(screen)).state, TurnState::AwaitingApproval);
    }

    #[test]
    fn claude_transcript_viewer_defers_to_quiescence() {
        let claude = manifest_for("claude").expect("claude manifest");
        let screen = "old transcript\nShowing detailed transcript\nctrl+o to toggle";
        let verdict = claude.detect(input(screen));
        assert_eq!(verdict.state, TurnState::Unknown);
        assert_eq!(verdict.rule.map(|(id, _)| id), Some("transcript_viewer"));
    }

    #[test]
    fn codex_working_footer_beats_stale_prompt() {
        let codex = manifest_for("codex").expect("codex manifest");
        let screen = "› an earlier prompt\n• Working (3s • esc to interrupt)";
        let verdict = codex.detect(input(screen));
        assert_eq!(verdict.state, TurnState::Busy);
        assert_eq!(verdict.rule.map(|(id, _)| id), Some("screen_working_fallback"));
    }

    #[test]
    fn codex_approval_dialog_after_prompt_marker() {
        let codex = manifest_for("codex").expect("codex manifest");
        let screen = "› run the command\n\n  $ touch /tmp/x.txt\n\n  Press enter to confirm or esc to cancel";
        assert_eq!(codex.detect(input(screen)).state, TurnState::AwaitingApproval);
    }

    #[test]
    fn osc_title_spinner_marks_codex_busy_and_plain_title_idle() {
        let codex = manifest_for("codex").expect("codex manifest");
        let screen = "› stale prompt from history";
        // Braille spinner in the OSC title → working, even with an idle screen.
        let busy = codex.detect(DetectionInput {
            screen,
            osc_title: "⠋ codex — thinking",
            osc_progress: "",
        });
        assert_eq!(busy.state, TurnState::Busy);
        assert_eq!(busy.rule.map(|(id, _)| id), Some("osc_title_working"));

        // "Action Required" title → blocked at top priority.
        let blocked = codex.detect(DetectionInput {
            screen,
            osc_title: "Action Required — codex",
            osc_progress: "",
        });
        assert_eq!(blocked.state, TurnState::AwaitingApproval);

        // Any other non-spinner title → idle.
        let idle = codex.detect(DetectionInput { screen, osc_title: "codex", osc_progress: "" });
        assert_eq!(idle.state, TurnState::AwaitingInput);
        assert_eq!(idle.rule.map(|(id, _)| id), Some("osc_title_idle"));
    }

    #[test]
    fn claude_osc_progress_zero_reads_idle() {
        let claude = manifest_for("claude").expect("claude manifest");
        let verdict =
            claude.detect(DetectionInput { screen: "", osc_title: "", osc_progress: "4;0" });
        assert_eq!(verdict.state, TurnState::AwaitingInput);
        assert_eq!(verdict.rule.map(|(id, _)| id), Some("osc_progress_idle"));
    }

    #[test]
    fn no_match_returns_unknown_not_idle() {
        // Deviation from herdr documented in the module docs: a manifest miss
        // defers to quiescence instead of assuming idle.
        let claude = manifest_for("claude").expect("claude manifest");
        let verdict = claude.detect(input("$ ls\nfile.txt\n$"));
        assert_eq!(verdict.state, TurnState::Unknown);
        assert_eq!(verdict.rule, None);
    }

    #[test]
    fn higher_priority_rule_wins_and_first_match_kept_on_tie() {
        let manifest = compile_manifest(
            r#"
id = "test"
[[rules]]
id = "low"
state = "idle"
priority = 10
contains = ["marker"]
[[rules]]
id = "tie-first"
state = "working"
priority = 50
contains = ["marker"]
[[rules]]
id = "tie-second"
state = "blocked"
priority = 50
contains = ["marker"]
"#,
        )
        .expect("test manifest");
        // Leak: tests need the 'static self the public API has.
        let manifest: &'static CompiledManifest = Box::leak(Box::new(manifest));
        let verdict = manifest.detect(input("marker"));
        assert_eq!(verdict.state, TurnState::Busy);
        assert_eq!(verdict.rule.map(|(id, _)| id), Some("tie-first"));
    }

    #[test]
    fn gate_semantics_all_any_not_and_line_regex() {
        let manifest = compile_manifest(
            r#"
id = "test"
[[rules]]
id = "gated"
state = "blocked"
priority = 1
contains = ["always"]
any = [ { contains = ["option a"] }, { contains = ["option b"] } ]
not = [ { contains = ["veto"] } ]
line_regex = ['^\s*❯']
"#,
        )
        .expect("test manifest");
        let manifest: &'static CompiledManifest = Box::leak(Box::new(manifest));
        let hit = "ALWAYS shown\noption b\n  ❯ pick";
        assert_eq!(manifest.detect(input(hit)).state, TurnState::AwaitingApproval);
        // `not` vetoes.
        let vetoed = format!("{hit}\nveto");
        assert_eq!(manifest.detect(input(&vetoed)).state, TurnState::Unknown);
        // `any` needs at least one branch.
        let no_option = "always\n  ❯ pick";
        assert_eq!(manifest.detect(input(no_option)).state, TurnState::Unknown);
        // `line_regex` must match a whole line, not the joined text.
        let no_prompt_line = "always ❯ inline\noption a";
        assert_eq!(manifest.detect(input(no_prompt_line)).state, TurnState::Unknown);
    }

    #[test]
    fn region_windows_slice_as_documented() {
        let content = "top\n\nmid\nlast1\n\nlast2";
        assert_eq!(bottom_non_empty_lines(content, 2), "last1\n\nlast2");
        assert_eq!(bottom_lines(content, 1), "last2");
        assert_eq!(top_non_empty_lines(content, 1), "top\n");
        assert_eq!(last_non_empty_line("a\nb\n\n"), "b");
    }

    #[test]
    fn prompt_box_regions_find_the_boxed_prompt() {
        let content = "history\n──────────\n❯ type here\n──────────\nfooter";
        assert_eq!(prompt_box_body(content), Some("❯ type here\n"));
        assert_eq!(above_prompt_box(content), "history\n");
        assert_eq!(after_last_horizontal_rule(content), "footer");
    }

    #[test]
    fn codex_prompt_marker_regions() {
        let content = "• block\n› current prompt\ntail";
        assert_eq!(after_last_prompt_marker(content), "tail");
        assert_eq!(before_current_prompt_marker(content), "• block\n");
        // A block marker after the prompt means the prompt is stale.
        let stale = "› old prompt\n• Working";
        assert_eq!(whole_recent_without_current_prompt_marker(stale), stale);
        assert_eq!(whole_recent_without_current_prompt_marker("› live prompt"), "");
    }

    #[test]
    fn override_slot_hot_reloads_edits_and_reverts_on_removal() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("claude.toml");
        let mut slot = OverrideSlot { last_checked: None, mtime: None, manifest: None };

        // Absent file → bundled stays authoritative.
        refresh_override_slot(&mut slot, &path, "claude");
        assert!(slot.manifest.is_none());

        // Valid override compiles and activates.
        std::fs::write(
            &path,
            "id = \"claude\"\n[[rules]]\nid = \"v1\"\nstate = \"working\"\ncontains = [\"override marker v1\"]\n",
        )
        .expect("write v1");
        refresh_override_slot(&mut slot, &path, "claude");
        let first = slot.manifest.expect("override should activate");
        assert_eq!(first.detect(input("override marker v1")).state, TurnState::Busy);

        // Unchanged mtime → the same compiled manifest is kept.
        refresh_override_slot(&mut slot, &path, "claude");
        assert!(std::ptr::eq(first, slot.manifest.expect("override still active")));

        // An edited file (newer mtime) recompiles — the hot-reload itself.
        std::fs::write(
            &path,
            "id = \"claude\"\n[[rules]]\nid = \"v2\"\nstate = \"blocked\"\ncontains = [\"override marker v2\"]\n",
        )
        .expect("write v2");
        bump_mtime(&path);
        refresh_override_slot(&mut slot, &path, "claude");
        let second = slot.manifest.expect("edited override should reload");
        assert_eq!(second.detect(input("override marker v2")).state, TurnState::AwaitingApproval);
        assert_eq!(second.detect(input("override marker v1")).state, TurnState::Unknown);

        // A broken edit keeps the previous compiled override active.
        std::fs::write(&path, "not toml at all").expect("write broken");
        bump_mtime(&path);
        refresh_override_slot(&mut slot, &path, "claude");
        assert!(std::ptr::eq(second, slot.manifest.expect("broken edit keeps previous")));

        // An id mismatch is refused the same way.
        std::fs::write(
            &path,
            "id = \"codex\"\n[[rules]]\nid = \"x\"\nstate = \"idle\"\ncontains = [\"y\"]\n",
        )
        .expect("write mismatched");
        bump_mtime(&path);
        refresh_override_slot(&mut slot, &path, "claude");
        assert!(std::ptr::eq(second, slot.manifest.expect("id mismatch keeps previous")));

        // Removing the file reverts to the bundled manifest.
        std::fs::remove_file(&path).expect("remove override");
        refresh_override_slot(&mut slot, &path, "claude");
        assert!(slot.manifest.is_none());
    }

    /// Filesystems with coarse mtime granularity could make two writes in the
    /// same instant look unchanged; force a strictly newer mtime.
    fn bump_mtime(path: &std::path::Path) {
        let file = std::fs::OpenOptions::new().write(true).open(path).expect("open for mtime");
        file.set_modified(SystemTime::now() + Duration::from_secs(2)).expect("bump mtime");
    }

    #[test]
    fn engine_version_gate_rejects_future_manifests() {
        let err = compile_manifest(
            r#"
id = "future"
min_engine_version = 99
[[rules]]
id = "r"
state = "idle"
contains = ["x"]
"#,
        )
        .expect_err("future manifest must be rejected");
        assert!(err.contains("requires engine 99"), "unexpected error: {err}");
    }

    #[test]
    fn skip_state_update_requires_unknown_state() {
        let err = compile_manifest(
            r#"
id = "bad"
[[rules]]
id = "r"
state = "idle"
skip_state_update = true
contains = ["x"]
"#,
        )
        .expect_err("skip_state_update with non-unknown state must be rejected");
        assert!(err.contains("skip_state_update"), "unexpected error: {err}");
    }
}
