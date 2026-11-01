//! Source locations a command's output cites (`path:line`), turned into
//! bounded `ReadFiles` ranges so a remote client can inspect or edit the cited
//! code without spending another round trip on a read.
//!
//! Remote reasoning models spend most of a session thinking between calls,
//! and the most common follow-up to a command is reading the code its output
//! points at: search hits (`rg -n`, `grep -n`), compiler diagnostics,
//! tracebacks. Only a handful of precise citations are resolved; a broad
//! listing is left alone, since attaching code nobody asked for costs context.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

/// More distinct locations than this means a listing, not a pointer.
pub const MAX_LOCATIONS: usize = 8;
/// More distinct files than this means a listing, not a pointer.
pub const MAX_FILES: usize = 4;
/// Lines shown on each side of a cited line.
pub const CONTEXT_LINES: usize = 8;
/// Visible bytes the attached excerpts may use in one result.
pub const BUDGET_BYTES: usize = 16 * 1024;
/// Only the tail of a long output is scanned: diagnostics and the last
/// failure are there, and the scan stays bounded.
const SCAN_BYTES: usize = 64 * 1024;
const MAX_LINE_BYTES: usize = 2048;
/// Ranges closer than this are merged into one excerpt.
const MERGE_GAP: usize = 3;

/// The `ReadFiles` specs (`/abs/path:start-end`) for the code `output` cites,
/// relative paths resolved against `cwd`. `None` when nothing resolvable is
/// cited or the output is a broad listing.
pub fn read_specs(output: &str, cwd: &Path) -> Option<Vec<String>> {
    let mut lines_by_file: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
    let mut locations = 0usize;
    for line in tail(output, SCAN_BYTES).lines().filter(|line| line.len() <= MAX_LINE_BYTES) {
        for (path, number) in citations(line) {
            let Some(path) = resolve(path, cwd) else { continue };
            let lines = lines_by_file.entry(path).or_default();
            if !lines.contains(&number) {
                lines.push(number);
                locations += 1;
            }
            if locations > MAX_LOCATIONS || lines_by_file.len() > MAX_FILES {
                return None;
            }
        }
    }
    let specs: Vec<String> = lines_by_file
        .into_iter()
        .flat_map(|(path, lines)| {
            merged_ranges(lines)
                .into_iter()
                .map(move |(start, end)| format!("{}:{start}-{end}", path.display()))
        })
        .collect();
    (!specs.is_empty()).then_some(specs)
}

/// `(path, line)` pairs one output line cites, in the conventions of common
/// tools: `path:line[:col]` (rg/grep -n, rustc, gcc, go, eslint, pytest),
/// `File "path", line N` (Python tracebacks) and `path(line[,col])` (tsc, MSVC).
fn citations(line: &str) -> impl Iterator<Item = (&str, usize)> {
    static PATTERNS: OnceLock<[Regex; 3]> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        let path = r"[A-Za-z0-9_./~+@-][A-Za-z0-9_./~+@-]*";
        [
            Regex::new(&format!(
                r#"(?:^|[\s(\[<'"=])({path}):([0-9]{{1,7}})(?::[0-9]{{1,5}})?(?::|$|\s|\))"#
            )),
            Regex::new(&format!(r#"File "({path})", line ([0-9]{{1,7}})"#)),
            Regex::new(&format!(r"(?:^|\s)({path})\(([0-9]{{1,7}})(?:,[0-9]{{1,5}})?\)")),
        ]
        .map(|pattern| pattern.unwrap_or_else(|_| unreachable!("static citation pattern")))
    });
    patterns.iter().flat_map(move |pattern| {
        pattern.captures_iter(line).filter_map(|captures| {
            let path = captures.get(1)?.as_str();
            let number = captures.get(2)?.as_str().parse::<usize>().ok()?;
            looks_like_source(path).then_some((path, number)).filter(|(_, n)| *n > 0)
        })
    })
}

/// A path with a file extension; rejects bare words, versions and times
/// (`v1.2:3`, `12:30`) that the generic pattern would otherwise accept.
fn looks_like_source(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some((stem, extension)) = name.rsplit_once('.') else { return false };
    !stem.is_empty()
        && (1..=10).contains(&extension.len())
        && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && !extension.bytes().all(|byte| byte.is_ascii_digit())
}

/// The canonical regular file a cited path names, if it exists. Policy
/// (workspace, allowed paths, mode) is enforced later by `ReadFiles` itself.
fn resolve(path: &str, cwd: &Path) -> Option<PathBuf> {
    let path = path.strip_prefix("./").unwrap_or(path);
    let candidate =
        if Path::new(path).is_absolute() { PathBuf::from(path) } else { cwd.join(path) };
    let canonical = std::fs::canonicalize(candidate).ok()?;
    canonical.is_file().then_some(canonical)
}

fn merged_ranges(mut lines: Vec<usize>) -> Vec<(usize, usize)> {
    lines.sort_unstable();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for line in lines {
        let start = line.saturating_sub(CONTEXT_LINES).max(1);
        let end = line + CONTEXT_LINES;
        match ranges.last_mut() {
            Some(last) if start <= last.1 + MERGE_GAP => last.1 = last.1.max(end),
            _ => ranges.push((start, end)),
        }
    }
    ranges
}

fn tail(text: &str, bytes: usize) -> &str {
    if text.len() <= bytes {
        return text;
    }
    let mut start = text.len() - bytes;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    // Drop the partial first line.
    text[start..].split_once('\n').map_or("", |(_, rest)| rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let root = dir.path().canonicalize().unwrap_or_else(|_| unreachable!());
        std::fs::create_dir_all(root.join("src/tools")).unwrap_or_else(|_| unreachable!());
        for file in ["src/main.rs", "src/tools/herdr.rs", "app.py", "web.ts"] {
            std::fs::write(root.join(file), "x\n".repeat(200)).unwrap_or_else(|_| unreachable!());
        }
        (dir, root)
    }

    fn spec(root: &Path, file: &str, start: usize, end: usize) -> String {
        format!("{}:{start}-{end}", root.join(file).display())
    }

    #[test]
    fn common_tool_conventions_resolve_to_context_ranges() {
        let (_dir, root) = workspace();
        let output = "\
src/tools/herdr.rs:120:    let id = binding.resolve(target)?;
error[E0308]: mismatched types
  --> src/main.rs:40:9
  File \"app.py\", line 7, in <module>
web.ts(90,3): error TS2322: Type 'string' is not assignable";
        assert_eq!(
            read_specs(output, &root).unwrap_or_default(),
            vec![
                spec(&root, "app.py", 1, 15),
                spec(&root, "src/main.rs", 32, 48),
                spec(&root, "src/tools/herdr.rs", 112, 128),
                spec(&root, "web.ts", 82, 98),
            ]
        );
    }

    #[test]
    fn nearby_citations_merge_and_duplicates_count_once() {
        let (_dir, root) = workspace();
        let output = "src/main.rs:10:a\nsrc/main.rs:20:b\nsrc/main.rs:10:again\nsrc/main.rs:80:c";
        assert_eq!(
            read_specs(output, &root).unwrap_or_default(),
            vec![spec(&root, "src/main.rs", 2, 28), spec(&root, "src/main.rs", 72, 88)]
        );
    }

    #[test]
    fn broad_listings_attach_nothing() {
        let (_dir, root) = workspace();
        let many_lines = (1..=(MAX_LOCATIONS + 1))
            .map(|n| format!("src/main.rs:{}:hit", n * 30))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(read_specs(&many_lines, &root), None);

        for extra in ["a.rs", "b.rs", "c.rs", "d.rs"] {
            std::fs::write(root.join(extra), "x\n").unwrap_or_else(|_| unreachable!());
        }
        let many_files = "a.rs:1:x\nb.rs:1:x\nc.rs:1:x\nd.rs:1:x\nsrc/main.rs:1:x";
        assert_eq!(read_specs(many_files, &root), None);
    }

    #[test]
    fn non_files_versions_and_times_are_ignored() {
        let (_dir, root) = workspace();
        let output = "\
started at 12:30:45
package v1.2:3 ok
missing.rs:10: nothing here
http://example.com:8080/x.html
src:40:no extension
src/main.rs:0:zero line";
        assert_eq!(read_specs(output, &root), None);
    }

    #[test]
    fn absolute_and_dot_relative_paths_resolve() {
        let (_dir, root) = workspace();
        let absolute = format!("{}:5:x", root.join("app.py").display());
        assert_eq!(read_specs(&absolute, &root), Some(vec![spec(&root, "app.py", 1, 13)]));
        assert_eq!(read_specs("./app.py:5:x", &root), Some(vec![spec(&root, "app.py", 1, 13)]));
    }

    #[test]
    fn only_the_tail_of_huge_output_is_scanned() {
        let (_dir, root) = workspace();
        let output = format!("app.py:5:early\n{}\nsrc/main.rs:50:late", "noise\n".repeat(20_000));
        assert_eq!(read_specs(&output, &root), Some(vec![spec(&root, "src/main.rs", 42, 58)]));
    }
}
