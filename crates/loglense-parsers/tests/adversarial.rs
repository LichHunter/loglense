//! Adversarial probes: a cyclic `extends` pair shipped as real pack files, a
//! ten-thousand-rule pack, and a classically catastrophic-backtracking
//! pattern that must compile-or-error without hanging (the `regex` crate is
//! linear-time by design).

use std::path::PathBuf;

use loglense_parsers::{
    load_pack, load_pack_dir, FeedOutcome, LineCtx, PackError, Parser, ParserRegistry,
    RegistryError, Source, Stream,
};
use tempfile::TempDir;

fn write(dir: &TempDir, name: &str, contents: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, contents).expect("write pack");
    path
}

#[test]
fn cyclic_pack_pair_from_disk_is_rejected_at_registry_resolve() {
    let tmp = TempDir::new().expect("tempdir");
    write(
        &tmp,
        "aaa.toml",
        "schema = 1\nname = 'aaa'\nextends = 'bbb'\n[[rule]]\nid = 'r'\nseverity = 'info'\npattern = '^a'\nmessage = 'a'\nkind = 'line'\n",
    );
    write(
        &tmp,
        "bbb.toml",
        "schema = 1\nname = 'bbb'\nextends = 'aaa'\n[[rule]]\nid = 'r'\nseverity = 'info'\npattern = '^b'\nmessage = 'b'\nkind = 'line'\n",
    );
    let packs = load_pack_dir(tmp.path());
    assert_eq!(packs.len(), 2, "both files load individually");
    assert!(
        packs.iter().all(|(_, result)| result.is_ok()),
        "extends is validated by the registry, not the loader"
    );

    let mut registry = ParserRegistry::new();
    for (_path, result) in packs {
        let pack = result.expect("loaded");
        let registrable = pack.clone();
        registry.register(
            pack.name(),
            move || Box::new(registrable.clone()),
            Source::ProjectPack,
            pack.extends().map(str::to_string),
        );
    }
    let err = registry.resolve().expect_err("cycle must fail at resolve");
    match &err {
        RegistryError::ExtendsCycle { cycle } => {
            assert!(
                cycle == "aaa → bbb → aaa" || cycle == "bbb → aaa → bbb",
                "cycle names its full path: {cycle}"
            );
        }
        other => panic!("wrong error: {other:?}"),
    }
    assert!(
        matches!(
            registry.build_chain("aaa"),
            Err(RegistryError::ExtendsCycle { .. })
        ),
        "chain building refuses the cycle too"
    );
}

#[test]
fn ten_thousand_rule_pack_is_rejected_fast_with_a_clear_error() {
    let tmp = TempDir::new().expect("tempdir");
    let mut toml = String::from("schema = 1\nname = 'megapack'\n");
    for i in 0..10_000 {
        toml.push_str(&format!(
            "[[rule]]\nid = 'r{i}'\nseverity = 'info'\npattern = '^token{i}$'\nmessage = 'm'\nkind = 'line'\n"
        ));
    }
    let path = write(&tmp, "mega.toml", &toml);
    let err = load_pack(&path).expect_err("10k rules must be rejected");
    match &err {
        PackError::TooManyRules { count, .. } => assert_eq!(*count, 10_000),
        other => panic!("wrong error: {other:?}"),
    }
    assert!(err.to_string().contains("10000"), "count is stated: {err}");
}

#[test]
fn pathological_backtracking_pattern_cannot_hang() {
    // "(a+)+b" is the classic exponential-backtracking killer under
    // backtracking engines. The regex crate compiles it to a linear-time
    // NFA, so a non-matching 100k-'a' line must return instantly.
    let tmp = TempDir::new().expect("tempdir");
    let path = write(
        &tmp,
        "evil.toml",
        "schema = 1\nname = 'evil'\n[[rule]]\nid = 'evil'\nseverity = 'error'\npattern = '^(?:a+)+b$'\nmessage = 'matched'\nkind = 'line'\n",
    );
    let pack = load_pack(&path).expect("linear-time dialect compiles");
    let mut state = pack.new_run();
    let line = "a".repeat(100_000);
    let outcome = state.feed(
        &line,
        &LineCtx {
            stream: Stream::Out,
            line_no: 1,
        },
    );
    assert_eq!(outcome, FeedOutcome::Passed, "no b, no match, no hang");
}

#[test]
fn empty_pack_file_is_a_parse_error_not_a_panic() {
    let tmp = TempDir::new().expect("tempdir");
    let path = write(&tmp, "empty.toml", "");
    let err = load_pack(&path).expect_err("empty is not a valid pack");
    assert!(matches!(err, PackError::Parse { .. }), "{err}");
}

#[test]
fn garbage_file_is_a_parse_error_not_a_panic() {
    let tmp = TempDir::new().expect("tempdir");
    let path = write(&tmp, "garbage.toml", "\x00not [valid toml {{{");
    let err = load_pack(&path).expect_err("garbage is not a valid pack");
    assert!(matches!(err, PackError::Parse { .. }), "{err}");
}

#[test]
fn missing_file_is_an_io_error_naming_the_path() {
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().join("absent.toml");
    let err = load_pack(&path).expect_err("missing file must fail");
    assert!(matches!(err, PackError::Io { .. }));
    assert!(err.to_string().contains("absent.toml"), "{err}");
}
