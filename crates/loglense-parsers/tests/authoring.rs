//! Pack-authoring support (T14): collect-all validation, claim tracing,
//! and the standard registry's broken-pack reporting.

use loglense_parsers::{standard_registry, validate_pack, LineCtx, PackError, Source, Stream};

const BROKEN_RULES: &str = r#"
schema = 1
name = "twobad"

[detect]
pattern = '('
min_score = 0

[[rule]]
id = "first"
severity = "error"
pattern = '[unclosed'
message = "x"
kind = "line"

[[rule]]
id = "fine"
severity = "info"
pattern = '^ok$'
message = "ok"
kind = "line"

[[rule]]
id = "second"
severity = "warning"
pattern = '(?P<dup>a)(?P<dup>b)'
message = "y"
kind = "line"
"#;

const GOOD: &str = r#"
schema = 1
name = "mytool"
extends = "java"

[[rule]]
id = "tool-error"
severity = "error"
pattern = '^\[ERR\] (.+)$'
message = "$1"
kind = "line"
"#;

#[test]
fn validate_reports_every_bad_pattern_and_field_not_just_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("twobad.toml");
    std::fs::write(&path, BROKEN_RULES).unwrap();
    let errors = validate_pack(&path);
    let rendered: Vec<String> = errors.iter().map(ToString::to_string).collect();
    assert_eq!(errors.len(), 4, "{rendered:#?}");
    assert!(matches!(errors[0], PackError::MinScore { .. }));
    assert!(rendered[1].contains("detect.pattern"), "{}", rendered[1]);
    assert!(
        rendered[2].contains("rule[id=first].pattern"),
        "{}",
        rendered[2]
    );
    assert!(
        rendered[3].contains("rule[id=second].pattern"),
        "{}",
        rendered[3]
    );
    assert!(rendered.iter().all(|e| e.contains("twobad.toml")));
}

#[test]
fn validate_accepts_a_good_pack_and_stops_at_toml_errors() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.toml");
    std::fs::write(&good, GOOD).unwrap();
    assert!(validate_pack(&good).is_empty());
    let bad = dir.path().join("bad.toml");
    std::fs::write(&bad, "schema = 1\nname = \"x\"\nfrobnicate = true\n").unwrap();
    let errors = validate_pack(&bad);
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].to_string().contains("frobnicate"),
        "{}",
        errors[0]
    );
}

#[test]
fn standard_registry_layers_packs_and_reports_broken_ones() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    let user = dir.path().join("user");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(project.join("mytool.toml"), GOOD).unwrap();
    std::fs::write(user.join("broken.toml"), "not toml [").unwrap();
    let loaded = standard_registry(&project, Some(&user));
    assert_eq!(loaded.broken.len(), 1);
    assert_eq!(loaded.broken[0].source, Source::UserPack);
    assert!(loaded.broken[0].path.ends_with("broken.toml"));
    let mytool = loaded
        .registry
        .registrations()
        .into_iter()
        .find(|r| r.name == "mytool")
        .expect("pack registered");
    assert_eq!(mytool.source, Source::ProjectPack);
    assert!(loaded.registry.names().contains(&"cargo"), "tools included");
}

#[test]
fn feed_traced_names_the_claiming_chain_member() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("mytool.toml"), GOOD).unwrap();
    let loaded = standard_registry(dir.path(), None);
    let chain = loaded.registry.build_chain("mytool").unwrap();
    let names = chain.parser_names();
    assert_eq!(names, ["mytool", "java", "generic"]);
    let mut state = chain.new_run();
    let ctx = |line_no| LineCtx {
        stream: Stream::Out,
        line_no,
    };
    let (events, claimer) = state.feed_traced("[ERR] disk full", &ctx(1));
    assert_eq!(claimer.map(|i| names[i]), Some("mytool"));
    assert_eq!(events[0].message, "disk full");
    let (_, claimer) = state.feed_traced("just prose", &ctx(2));
    assert_eq!(claimer.map(|i| names[i]), Some("generic"));
}
