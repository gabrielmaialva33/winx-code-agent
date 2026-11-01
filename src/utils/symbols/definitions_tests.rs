#![allow(clippy::unwrap_used)]
use super::definitions::{resolve, FailureReason};
use std::fmt::Write as _;

#[test]
fn rust_qualified_methods_include_attached_docs_attributes_and_all_duplicates() {
    let source = "struct Parser;\nimpl Parser {\n    /// Parse docs.\n    #[inline]\n    fn parse(&self) {\n        let x = 1;\n    }\n}\nstruct Other;\nimpl Other { fn parse(&self) {} }\n";
    let ranges = resolve("rs", source, "Parser::parse").unwrap();
    assert_eq!(ranges.len(), 1);
    assert_eq!((ranges[0].start_line, ranges[0].end_line), (3, 7));
    assert_eq!(resolve("rs", source, "Parser.parse").unwrap(), ranges);
    assert_eq!(resolve("rs", source, "parse").unwrap().len(), 2);
    assert_eq!(
        resolve("rs", source, "OtherParser::parse").unwrap_err().reason,
        FailureReason::NotFound
    );
}

#[test]
fn python_decorators_and_qualified_nested_methods_are_exact() {
    let source = "class Parser:\n    # Parse docs.\n    @classmethod\n    def parse(cls, value):\n        return value\n\n    def unrelated(self):\n        return 0\n\ndef parse(value):\n    return value\n";
    let ranges = resolve("py", source, "Parser.parse").unwrap();
    assert_eq!(ranges.len(), 1);
    assert_eq!((ranges[0].start_line, ranges[0].end_line), (2, 5));
    assert_eq!(resolve("py", source, "parse").unwrap().len(), 2);
}

#[test]
fn comments_with_blank_lines_and_previous_item_trailing_docs_do_not_attach() {
    let source = "/// Not attached.\n\nfn target() {}\nfn other() {} /// Previous trailing doc.\nfn next() {}\n";
    assert_eq!(resolve("rs", source, "target").unwrap()[0].start_line, 3);
    assert_eq!(resolve("rs", source, "next").unwrap()[0].start_line, 5);
}

#[test]
fn go_receiver_and_js_object_methods_have_owners() {
    let go = "package main\ntype Parser struct {}\n// Parse docs.\nfunc (p *Parser) Parse() int {\n return 1\n}\n";
    let range = &resolve("go", go, "Parser.Parse").unwrap()[0];
    assert_eq!((range.start_line, range.end_line), (3, 6));
    let js = "const Parser = {\n  /** Parse docs. */\n  parse(value) { return value; }\n};\n";
    let range = &resolve("js", js, "Parser.parse").unwrap()[0];
    assert_eq!((range.start_line, range.end_line), (2, 3));
}

#[test]
fn every_enabled_tags_language_resolves_a_definition_range() {
    let mut fixtures = vec![
        ("rs", "fn target() {}"),
        ("py", "def target():\n    pass\n"),
        ("js", "function target() {}"),
        ("ts", "export function target(): void {}"),
        ("tsx", "export function target() { return <div/>; }"),
        ("go", "package p\nfunc target() {}"),
        ("c", "int target(void) { return 0; }"),
        ("java", "class Parser { void target() {} }"),
    ];
    fixtures.extend([
        ("cpp", "int target() { return 0; }"),
        ("cs", "class Parser { void target() {} }"),
        ("rb", "def target\n  0\nend\n"),
        ("php", "<?php function target() { return 0; }"),
        ("lua", "local function target()\n return 0\nend\n"),
        ("ex", "defmodule Parser do\n def target(), do: 0\nend\n"),
    ]);
    for (ext, source) in fixtures.into_iter().filter(|(ext, _)| super::supports(ext)) {
        let result = resolve(ext, source, "target");
        assert!(result.is_ok(), "{ext}: {result:?}");
        let ranges = result.unwrap();
        assert_eq!(ranges.len(), 1, "{ext}: {ranges:?}");
        assert!(ranges[0].start_line >= 1 && ranges[0].end_line <= source.lines().count());
    }
}

#[test]
fn unknown_unsupported_parse_failure_and_candidate_cap_are_explicit() {
    assert_eq!(
        resolve("rs", "fn known() {}", "missing").unwrap_err().reason,
        FailureReason::NotFound
    );
    assert_eq!(
        resolve("json", "{}", "missing").unwrap_err().reason,
        FailureReason::UnsupportedLanguage
    );
    assert_eq!(
        resolve("rs", "fn broken(", "broken").unwrap_err().reason,
        FailureReason::ParseFailed
    );
    assert_eq!(
        resolve("rs", "fn known() {}", "").unwrap_err().reason,
        FailureReason::InvalidSelector
    );
    let source = (0..17).fold(String::new(), |mut out, i| {
        writeln!(&mut out, "impl P{i} {{ fn target(&self) {{}} }}").unwrap();
        out
    });
    let failure = resolve("rs", &source, "target").unwrap_err();
    assert_eq!(failure.reason, FailureReason::TooManyMatches);
    assert_eq!(failure.candidates.len(), 16);
    assert!(failure.candidates_truncated);
    assert_eq!(
        resolve("rs", &" ".repeat(2_000_001), "target").unwrap_err().reason,
        FailureReason::SourceTooLarge
    );
}

#[test]
fn exported_arrows_templates_and_decorated_ts_methods_keep_metadata() {
    let ts="/** Target docs. */\nexport const target = (value: number) => {\n return value;\n};\nconst other = () => 0;\n";
    let range = &resolve("ts", ts, "target").unwrap()[0];
    assert_eq!((range.start_line, range.end_line), (1, 4));
    #[cfg(feature = "languages-extra")]
    {
        let cpp =
        "/** Template docs. */\ntemplate <typename T>\nT target(T value) {\n return value;\n}\n";
        let range = &resolve("cpp", cpp, "target").unwrap()[0];
        assert_eq!((range.start_line, range.end_line), (1, 5));
    }
    let ts="class Parser {\n /** Method docs. */\n @trace\n parse(value: number) { return value; }\n}\n";
    let range = &resolve("ts", ts, "Parser.parse").unwrap()[0];
    assert_eq!((range.start_line, range.end_line), (2, 4));
}

#[test]
fn unicode_crlf_and_generic_owner_suffixes_preserve_source_lines() {
    let source="mod input {\r\nstruct Parser<T>(T);\r\nimpl<T> Parser<T> {\r\n /// Café.\r\n fn parse(&self) { let café = 1; }\r\n}\r\n}\r\n";
    let range = &resolve("rs", source, "Parser::parse").unwrap()[0];
    assert_eq!((range.start_line, range.end_line), (4, 5));
}

#[test]
fn exact_match_cap_and_owner_depth_limits_are_explicit() {
    for count in [8, 9] {
        let source = (0..count).fold(String::new(), |mut out, id| {
            writeln!(&mut out, "impl P{id} {{ fn target(&self) {{}} }}").unwrap();
            out
        });
        let result = resolve("rs", &source, "target");
        if count == 8 {
            assert_eq!(result.unwrap().len(), 8);
        } else {
            let failure = result.unwrap_err();
            assert_eq!(failure.reason, FailureReason::TooManyMatches);
            assert_eq!(failure.candidates.len(), 9);
            assert!(!failure.candidates_truncated);
        }
    }
    let nested = format!("{}fn target() {{}}{}", "mod nested {".repeat(140), "}".repeat(140));
    assert_eq!(resolve("rs", &nested, "target").unwrap_err().reason, FailureReason::BudgetExceeded);
}
