//! Built-in registration wiring (task T7 deliverable e): `bases::register`
//! installs generic/json/logfmt as `Source::Builtin`, chains get `generic`
//! appended last, and autodetection picks the structured base on structured
//! samples.

use loglense_parsers::bases::{generic::GenericParser, json::JsonParser, logfmt::LogfmtParser};
use loglense_parsers::{
    bases, detect_from_sample, LineCtx, Parser, ParserRegistry, Registration, Source, Stream,
};

fn registry() -> ParserRegistry {
    let mut registry = ParserRegistry::new();
    bases::register(&mut registry);
    registry
}

#[test]
fn bases_register_as_builtin_with_no_extends() {
    let registry = registry();
    let rows: Vec<Registration> = registry
        .registrations()
        .into_iter()
        .filter(|row| matches!(row.name, "generic" | "json" | "logfmt"))
        .collect();
    assert_eq!(rows.len(), 3, "all three bases registered");
    for row in &rows {
        assert_eq!(row.source, Source::Builtin, "{}", row.name);
        assert_eq!(row.extends, None, "{}", row.name);
    }
    registry
        .resolve()
        .expect("no extends declared, resolves clean");
}

#[test]
fn chains_append_generic_last() {
    let registry = registry();
    assert_eq!(
        registry.build_chain("json").expect("chain").parser_names(),
        vec!["json", "generic"]
    );
    assert_eq!(
        registry
            .build_chain("logfmt")
            .expect("chain")
            .parser_names(),
        vec!["logfmt", "generic"]
    );
    assert_eq!(
        registry
            .build_chain("generic")
            .expect("chain")
            .parser_names(),
        vec!["generic"]
    );
}

#[test]
fn json_chain_falls_through_to_generic_on_prose() {
    let registry = registry();
    let mut state = registry.build_chain("json").expect("chain").new_run();
    let ctx = LineCtx {
        stream: Stream::Out,
        line_no: 1,
    };
    let events = state.feed("just words", &ctx);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].message, "just words");
    assert_eq!(
        events[0].severity,
        loglense_parsers::Severity::Info,
        "generic claimed the line json passed"
    );
}

#[test]
fn autodetect_prefers_the_matching_structured_base() {
    let registry = registry();
    let json_sample = [
        r#"{"level":"info","msg":"a"}"#,
        r#"{"level":"error","msg":"b"}"#,
    ];
    assert_eq!(detect_from_sample(&registry, &json_sample), "json");
    let logfmt_sample = ["level=info msg=a", "level=error msg=b"];
    assert_eq!(detect_from_sample(&registry, &logfmt_sample), "logfmt");
    assert_eq!(
        detect_from_sample(&registry, &["just", "some", "prose"]),
        "generic"
    );
}

#[test]
fn generic_detect_is_always_zero() {
    let sample = ["ERROR: total meltdown", "FATAL: worse"];
    for parser in [&GenericParser as &dyn Parser, &JsonParser, &LogfmtParser] {
        if parser.name() == "generic" {
            assert_eq!(parser.detect(&sample), 0, "generic never competes");
        }
    }
}
