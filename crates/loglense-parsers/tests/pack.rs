//! TOML pack loader (docs/parsers.md §4): schema-1 grammar, strict fields,
//! load-time regex compilation with `file:rule[id=…].pattern` errors, capture
//! interpolation, synthesized detection, stateless first-match intake, and
//! directory loading with bad-pack skip.

mod common;

use std::path::{Path, PathBuf};

use common::{MockBehavior, MockParser};
use loglense_parsers::{
    load_pack, load_pack_dir, FeedOutcome, LineCtx, PackError, Parser, ParserRegistry, Source,
    Stream,
};
use tempfile::TempDir;

/// The worked example from docs/parsers.md §4.4, verbatim in structure.
const MYTOOL_TOML: &str = r#"
schema = 1
name = "mytool"
extends = "java"
description = "internal build tool"

[detect]
pattern = '^mytool \['
min_score = 3

[[rule]]
id = "tool-error"
severity = "error"
pattern = '^\[ERR\] (.+)$'
message = "$1"
kind = "line"

[[rule]]
id = "build-failed"
severity = "error"
pattern = '^mytool: BUILD FAILED in (\d+)s$'
message = "build failed in $1 s"
kind = "summary"

[[rule]]
id = "test-failed"
severity = "error"
pattern = '^mytool test ([\w/]+) \.\.\. FAILED$'
message = "test $1 failed"
kind = "test"
"#;

fn write_pack(dir: &Path, file_name: &str, contents: &str) -> PathBuf {
    let path = dir.join(file_name);
    std::fs::write(&path, contents).expect("write pack");
    path
}

fn ctx(stream: Stream, line_no: u64) -> LineCtx {
    LineCtx { stream, line_no }
}

fn load_mytool() -> loglense_parsers::Pack {
    let tmp = TempDir::new().expect("tempdir");
    let path = write_pack(tmp.path(), "mytool.toml", MYTOOL_TOML);
    load_pack(&path).expect("docs example pack must load")
}

#[test]
fn valid_pack_loads_with_header_and_ordered_rules() {
    let pack = load_mytool();
    assert_eq!(pack.name(), "mytool");
    assert_eq!(pack.extends(), Some("java"));
    assert_eq!(pack.description(), Some("internal build tool"));
    let rules = pack.rules();
    assert_eq!(rules.len(), 3);
    assert_eq!(rules[0].id, "tool-error");
    assert_eq!(rules[0].severity, loglense_parsers::Severity::Error);
    assert_eq!(rules[0].kind, loglense_parsers::Kind::Line);
    assert_eq!(rules[2].id, "test-failed");
    assert_eq!(rules[2].kind, loglense_parsers::Kind::Test);
}

#[test]
fn unknown_header_field_error_names_file_and_field() {
    let tmp = TempDir::new().expect("tempdir");
    let path = write_pack(
        tmp.path(),
        "bad.toml",
        "schema = 1\nname = 'x'\nfrobnicate = true\n",
    );
    let err = load_pack(&path).expect_err("unknown field must fail");
    assert!(
        err.to_string().contains("bad.toml"),
        "names the file: {err}"
    );
    assert!(
        err.to_string().contains("frobnicate"),
        "names the field: {err}"
    );
}

#[test]
fn unknown_rule_field_error_names_file_and_field() {
    let tmp = TempDir::new().expect("tempdir");
    let path = write_pack(
        tmp.path(),
        "bad.toml",
        "schema = 1\nname = 'x'\n[[rule]]\nid = 'r'\nseverity = 'error'\npattern = 'x'\nmessage = 'm'\nkind = 'line'\ncolour = 'red'\n",
    );
    let err = load_pack(&path).expect_err("unknown rule field must fail");
    assert!(err.to_string().contains("bad.toml"), "{err}");
    assert!(err.to_string().contains("colour"), "{err}");
}

#[test]
fn bad_regex_error_names_rule_id_and_field() {
    let tmp = TempDir::new().expect("tempdir");
    let path = write_pack(
        tmp.path(),
        "broken.toml",
        "schema = 1\nname = 'x'\n[[rule]]\nid = 'error-line'\nseverity = 'error'\npattern = '('\nmessage = 'm'\nkind = 'line'\n",
    );
    let err = load_pack(&path).expect_err("bad regex must fail");
    assert!(matches!(err, PackError::BadRegex { .. }));
    assert!(
        err.to_string()
            .contains("broken.toml:rule[id=error-line].pattern"),
        "error is file:rule[id=…].pattern shaped: {err}"
    );
}

#[test]
fn bad_detect_regex_error_names_field() {
    let tmp = TempDir::new().expect("tempdir");
    let path = write_pack(
        tmp.path(),
        "broken.toml",
        "schema = 1\nname = 'x'\n[detect]\npattern = '('\nmin_score = 2\n",
    );
    let err = load_pack(&path).expect_err("bad detect regex must fail");
    assert!(
        err.to_string().contains("broken.toml:detect.pattern"),
        "{err}"
    );
}

#[test]
fn bad_severity_value_is_rejected() {
    let tmp = TempDir::new().expect("tempdir");
    let path = write_pack(
        tmp.path(),
        "bad.toml",
        "schema = 1\nname = 'x'\n[[rule]]\nid = 'r'\nseverity = 'fatal'\npattern = 'x'\nmessage = 'm'\nkind = 'line'\n",
    );
    let err = load_pack(&path).expect_err("bad severity must fail");
    assert!(err.to_string().contains("fatal"), "{err}");
    assert!(err.to_string().contains("bad.toml"), "{err}");
}

#[test]
fn bad_kind_value_is_rejected() {
    let tmp = TempDir::new().expect("tempdir");
    let path = write_pack(
        tmp.path(),
        "bad.toml",
        "schema = 1\nname = 'x'\n[[rule]]\nid = 'r'\nseverity = 'error'\npattern = 'x'\nmessage = 'm'\nkind = 'verbose'\n",
    );
    let err = load_pack(&path).expect_err("bad kind must fail");
    assert!(err.to_string().contains("verbose"), "{err}");
}

#[test]
fn schema_two_is_rejected() {
    let tmp = TempDir::new().expect("tempdir");
    let path = write_pack(tmp.path(), "future.toml", "schema = 2\nname = 'x'\n");
    let err = load_pack(&path).expect_err("schema=2 must fail");
    assert!(
        err.to_string().contains("unsupported schema version 2"),
        "{err}"
    );
}

#[test]
fn missing_required_fields_are_rejected() {
    let tmp = TempDir::new().expect("tempdir");
    for (label, contents, field) in [
        ("no name", "schema = 1\n", "name"),
        ("no schema", "name = 'x'\n", "schema"),
        (
            "rule missing kind",
            "schema = 1\nname = 'x'\n[[rule]]\nid = 'r'\nseverity = 'error'\npattern = 'p'\nmessage = 'm'\n",
            "kind",
        ),
        (
            "detect missing min_score",
            "schema = 1\nname = 'x'\n[detect]\npattern = 'p'\n",
            "min_score",
        ),
    ] {
        let path = write_pack(tmp.path(), "m.toml", contents);
        let Err(err) = load_pack(&path) else {
            panic!("{label} must fail");
        };
        assert!(err.to_string().contains(field), "{label}: {err}");
    }
}

#[test]
fn min_score_below_one_is_rejected() {
    let tmp = TempDir::new().expect("tempdir");
    let path = write_pack(
        tmp.path(),
        "zero.toml",
        "schema = 1\nname = 'x'\n[detect]\npattern = 'p'\nmin_score = 0\n",
    );
    let err = load_pack(&path).expect_err("min_score=0 must fail");
    assert!(err.to_string().contains("min_score"), "{err}");
}

#[test]
fn capture_interpolation_dollar_groups_and_literal() {
    let tmp = TempDir::new().expect("tempdir");
    let path = write_pack(
        tmp.path(),
        "interp.toml",
        "schema = 1\nname = 'interp'\n[[rule]]\nid = 'r'\nseverity = 'warning'\npattern = '^E(\\d+): (.*)$'\nmessage = 'code $1: $2 ($$literal)'\nkind = 'line'\n",
    );
    let pack = load_pack(&path).expect("pack loads");
    let mut state = pack.new_run();
    match state.feed("E42: boom", &ctx(Stream::Out, 1)) {
        FeedOutcome::Claimed(events) => {
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].message, "code 42: boom ($literal)");
        }
        FeedOutcome::Passed => panic!("must claim"),
    }
}

#[test]
fn unmatched_optional_group_interpolates_empty() {
    let tmp = TempDir::new().expect("tempdir");
    let path = write_pack(
        tmp.path(),
        "opt.toml",
        "schema = 1\nname = 'opt'\n[[rule]]\nid = 'r'\nseverity = 'info'\npattern = '^(?:WARN )?(.*)$'\nmessage = '<$1>'\nkind = 'line'\n",
    );
    let pack = load_pack(&path).expect("pack loads");
    let mut state = pack.new_run();
    match state.feed("bare", &ctx(Stream::Out, 1)) {
        FeedOutcome::Claimed(events) => assert_eq!(events[0].message, "<bare>"),
        FeedOutcome::Passed => panic!("must claim"),
    }
}

#[test]
fn feed_first_matching_rule_wins() {
    let tmp = TempDir::new().expect("tempdir");
    let path = write_pack(
        tmp.path(),
        "order.toml",
        "schema = 1\nname = 'order'\n[[rule]]\nid = 'first'\nseverity = 'warning'\npattern = '^rule '\nmessage = 'first'\nkind = 'line'\n[[rule]]\nid = 'second'\nseverity = 'error'\npattern = '^rule (.+)$'\nmessage = 'second $1'\nkind = 'line'\n",
    );
    let pack = load_pack(&path).expect("pack loads");
    let mut state = pack.new_run();
    match state.feed("rule fires both", &ctx(Stream::Out, 3)) {
        FeedOutcome::Claimed(events) => {
            assert_eq!(events[0].message, "first");
            assert_eq!(
                events[0].severity,
                loglense_parsers::Severity::Warning,
                "earlier rule wins, not the more specific later one"
            );
        }
        FeedOutcome::Passed => panic!("must claim"),
    }
}

#[test]
fn feed_without_match_passes_and_finish_is_empty() {
    let pack = load_mytool();
    let mut state = pack.new_run();
    assert_eq!(
        state.feed("nothing matches this prose", &ctx(Stream::Out, 9)),
        FeedOutcome::Passed
    );
    assert!(state.finish().is_empty(), "pack rules are stateless");
}

#[test]
fn feed_stamps_stream_and_line_number() {
    let pack = load_mytool();
    let mut state = pack.new_run();
    match state.feed("[ERR] connection refused", &ctx(Stream::Err, 12)) {
        FeedOutcome::Claimed(events) => {
            assert_eq!(events[0].stream, Stream::Err);
            assert_eq!(events[0].lines, vec![12]);
            assert_eq!(events[0].message, "connection refused");
            assert_eq!(events[0].kind, loglense_parsers::Kind::Line);
        }
        FeedOutcome::Passed => panic!("must claim"),
    }
}

#[test]
fn detect_synthesis_scores_follow_docs_formula() {
    let tmp = TempDir::new().expect("tempdir");
    let path = write_pack(
        tmp.path(),
        "mv.toml",
        "schema = 1\nname = 'mv'\n[detect]\npattern = '^MV '\nmin_score = 3\n",
    );
    let pack = load_pack(&path).expect("pack loads");
    let score_for = |n: usize| {
        let lines: Vec<String> = (0..n).map(|i| format!("MV line {i}")).collect();
        let sample: Vec<&str> = lines.iter().map(String::as_str).collect();
        pack.detect(&sample)
    };
    let noise = ["plain prose", "more prose"];
    assert_eq!(pack.detect(&noise), 0, "zero matches score zero");
    assert_eq!(score_for(1), 34, "ceil(100*1/3)");
    assert_eq!(score_for(2), 67, "ceil(100*2/3)");
    assert_eq!(score_for(3), 100, "full confidence at threshold");
    assert_eq!(score_for(10), 100, "capped at 100");
}

#[test]
fn pack_without_detect_section_never_autodetects() {
    let tmp = TempDir::new().expect("tempdir");
    let path = write_pack(
        tmp.path(),
        "plain.toml",
        "schema = 1\nname = 'plain'\n[[rule]]\nid = 'r'\nseverity = 'info'\npattern = '^hi$'\nmessage = 'hi'\nkind = 'line'\n",
    );
    let pack = load_pack(&path).expect("pack loads");
    assert_eq!(pack.detect(&["hi", "hi", "hi"]), 0);
}

#[test]
fn pack_registers_and_extends_into_registry_chains() {
    let pack = load_mytool();
    let mut registry = ParserRegistry::new();
    registry.register(
        "generic",
        MockParser::new("generic")
            .with_behavior(MockBehavior::ClaimAll)
            .ctor(),
        Source::Builtin,
        None,
    );
    let registrable = pack.clone();
    registry.register(
        "mytool",
        move || Box::new(registrable.clone()),
        Source::ProjectPack,
        Some("java".to_string()),
    );
    registry.register(
        "java",
        MockParser::new("java").ctor(),
        Source::Builtin,
        None,
    );
    registry.resolve().expect("mytool → java resolves");
    let chain = registry.build_chain("mytool").expect("chain builds");
    assert_eq!(chain.parser_names(), vec!["mytool", "java", "generic"]);

    let mut state = chain.new_run();
    let events = state.feed("[ERR] nope", &ctx(Stream::Out, 1));
    assert_eq!(events.len(), 1, "pack rule claims via the chain");
    assert_eq!(events[0].message, "nope");
}

#[test]
fn too_many_rules_rejected_at_load() {
    let tmp = TempDir::new().expect("tempdir");
    let mut big = String::from("schema = 1\nname = 'big'\n");
    for i in 0..1001 {
        big.push_str(&format!(
            "[[rule]]\nid = 'r{i}'\nseverity = 'info'\npattern = '^p{i}$'\nmessage = 'm'\nkind = 'line'\n"
        ));
    }
    let path = write_pack(tmp.path(), "big.toml", &big);
    let err = load_pack(&path).expect_err("1001 rules must fail");
    assert!(err.to_string().contains("1001"), "{err}");

    let mut ok = String::from("schema = 1\nname = 'ok'\n");
    for i in 0..1000 {
        ok.push_str(&format!(
            "[[rule]]\nid = 'r{i}'\nseverity = 'info'\npattern = '^p{i}$'\nmessage = 'm'\nkind = 'line'\n"
        ));
    }
    let ok_path = write_pack(tmp.path(), "ok.toml", &ok);
    load_pack(&ok_path).expect("exactly 1000 rules is allowed");
}

#[test]
fn load_pack_dir_skips_broken_packs_and_loads_the_rest() {
    let tmp = TempDir::new().expect("tempdir");
    write_pack(tmp.path(), "good.toml", "schema = 1\nname = 'good'\n");
    write_pack(
        tmp.path(),
        "broken.toml",
        "schema = 1\nname = 'broken'\n[[rule]]\nid = 'r'\nseverity = 'error'\npattern = '('\nmessage = 'm'\nkind = 'line'\n",
    );
    write_pack(tmp.path(), "notes.txt", "not a pack");
    write_pack(tmp.path(), "future.toml", "schema = 9\nname = 'future'\n");

    let results = load_pack_dir(tmp.path());
    assert_eq!(
        results.len(),
        3,
        "only *.toml considered: {:?}",
        results.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
    let good = results
        .iter()
        .find(|(p, _)| p.file_name().is_some_and(|n| n == "good.toml"))
        .expect("good.toml present");
    assert!(good.1.is_ok(), "good pack loads");
    let broken = results
        .iter()
        .find(|(p, _)| p.file_name().is_some_and(|n| n == "broken.toml"))
        .expect("broken.toml present");
    assert!(
        broken.1.is_err(),
        "broken pack reported, not silently dropped"
    );
    assert!(matches!(broken.1, Err(PackError::BadRegex { .. })));
    let future = results
        .iter()
        .find(|(p, _)| p.file_name().is_some_and(|n| n == "future.toml"))
        .expect("future.toml present");
    assert!(matches!(future.1, Err(PackError::UnsupportedSchema { .. })));
}

#[test]
fn load_pack_dir_missing_directory_is_empty() {
    let tmp = TempDir::new().expect("tempdir");
    assert!(load_pack_dir(&tmp.path().join("formats.d")).is_empty());
}
