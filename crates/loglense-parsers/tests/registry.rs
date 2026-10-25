//! Registry behavior (docs/parsers.md §3): source precedence on duplicate
//! names, `extends` validation (unknown parent, cycles), and chain
//! construction order (selected → ancestors base-most → generic last).

mod common;

use common::MockParser;
use loglense_parsers::{ParserRegistry, RegistryError, Source};

fn mock(name: &'static str, extends: Option<&'static str>) -> MockParser {
    match extends {
        Some(parent) => MockParser::new(name).with_extends(parent),
        None => MockParser::new(name),
    }
}

fn registry_with(entries: &[(&'static str, Option<&'static str>)]) -> ParserRegistry {
    let mut registry = ParserRegistry::new();
    for (name, extends) in entries {
        registry.register(
            name,
            mock(name, *extends).ctor(),
            Source::Builtin,
            extends.map(str::to_string),
        );
    }
    registry
}

fn source_of(registry: &ParserRegistry, name: &str) -> Source {
    registry
        .registrations()
        .iter()
        .find(|r| r.name == name)
        .map(|r| r.source)
        .expect("parser registered")
}

#[test]
fn higher_precedence_replaces_lower() {
    let mut registry = ParserRegistry::new();
    registry.register("x", MockParser::new("x").ctor(), Source::Builtin, None);
    registry.register("x", MockParser::new("x").ctor(), Source::ProjectPack, None);
    assert_eq!(source_of(&registry, "x"), Source::ProjectPack);
    assert_eq!(
        registry.registrations().len(),
        1,
        "replacement, not addition"
    );
}

#[test]
fn lower_precedence_arriving_later_is_ignored() {
    let mut registry = ParserRegistry::new();
    registry.register("x", MockParser::new("x").ctor(), Source::UserPack, None);
    registry.register("x", MockParser::new("x").ctor(), Source::Builtin, None);
    assert_eq!(source_of(&registry, "x"), Source::UserPack);
    assert_eq!(registry.registrations().len(), 1);
}

#[test]
fn same_level_duplicate_later_registration_wins() {
    let mut registry = ParserRegistry::new();
    registry.register(
        "x",
        mock("x", Some("java")).ctor(),
        Source::Builtin,
        Some("java".to_string()),
    );
    registry.register(
        "x",
        mock("x", Some("maven")).ctor(),
        Source::Builtin,
        Some("maven".to_string()),
    );
    assert_eq!(
        registry.registrations().len(),
        1,
        "duplicate replaced in place"
    );
    let x = registry
        .registrations()
        .into_iter()
        .find(|r| r.name == "x")
        .expect("x registered");
    assert_eq!(
        x.extends,
        Some("maven"),
        "later same-level registration wins"
    );
}

#[test]
fn precedence_is_project_over_user_over_builtin() {
    let mut registry = ParserRegistry::new();
    for source in [Source::Builtin, Source::UserPack, Source::ProjectPack] {
        registry.register("x", MockParser::new("x").ctor(), source, None);
    }
    assert_eq!(source_of(&registry, "x"), Source::ProjectPack);
}

#[test]
fn unknown_extends_error_names_parent_and_child() {
    let registry = registry_with(&[("child", Some("ghost"))]);
    let err = registry.resolve().expect_err("unknown parent must fail");
    match &err {
        RegistryError::UnknownExtends { parent, child } => {
            assert_eq!(parent, "ghost");
            assert_eq!(child, "child");
        }
        other => panic!("wrong error: {other:?}"),
    }
    assert!(err.to_string().contains("ghost"));
    assert!(err.to_string().contains("child"));
}

#[test]
fn extends_cycle_error_names_the_cycle_path() {
    let registry = registry_with(&[("a", Some("b")), ("b", Some("a"))]);
    let err = registry.resolve().expect_err("cycle must fail");
    match &err {
        RegistryError::ExtendsCycle { cycle } => assert_eq!(cycle, "a → b → a"),
        other => panic!("wrong error: {other:?}"),
    }
}

#[test]
fn self_extends_is_a_cycle() {
    let registry = registry_with(&[("a", Some("a"))]);
    let err = registry.resolve().expect_err("self-extends must fail");
    match &err {
        RegistryError::ExtendsCycle { cycle } => assert_eq!(cycle, "a → a"),
        other => panic!("wrong error: {other:?}"),
    }
}

#[test]
fn deep_cycle_detected_from_descendant() {
    let registry = registry_with(&[("c", Some("a")), ("a", Some("b")), ("b", Some("a"))]);
    let err = registry.resolve().expect_err("cycle must fail");
    match &err {
        RegistryError::ExtendsCycle { cycle } => assert_eq!(cycle, "a → b → a"),
        other => panic!("wrong error: {other:?}"),
    }
}

#[test]
fn valid_hierarchy_resolves() {
    let registry = registry_with(&[("generic", None), ("java", None), ("maven", Some("java"))]);
    registry.resolve().expect("maven → java → ok");
}

#[test]
fn chain_order_is_selected_then_ancestors_then_generic() {
    let registry = registry_with(&[("generic", None), ("java", None), ("maven", Some("java"))]);
    let chain = registry.build_chain("maven").expect("chain builds");
    assert_eq!(chain.parser_names(), vec!["maven", "java", "generic"]);
}

#[test]
fn explicit_generic_selection_yields_just_generic() {
    let registry = registry_with(&[("generic", None), ("java", None)]);
    let chain = registry.build_chain("generic").expect("generic selectable");
    assert_eq!(chain.parser_names(), vec!["generic"]);
}

#[test]
fn chain_appends_generic_only_once() {
    let registry = registry_with(&[("generic", None), ("mytool", Some("generic"))]);
    let chain = registry.build_chain("mytool").expect("chain builds");
    assert_eq!(chain.parser_names(), vec!["mytool", "generic"]);
}

#[test]
fn chain_without_generic_registered_omits_it() {
    let registry = registry_with(&[("java", None), ("maven", Some("java"))]);
    let chain = registry.build_chain("maven").expect("chain builds");
    assert_eq!(chain.parser_names(), vec!["maven", "java"]);
}

#[test]
fn unknown_selection_errors() {
    let registry = registry_with(&[("generic", None)]);
    let err = registry.build_chain("nope").expect_err("unknown parser");
    assert!(matches!(err, RegistryError::UnknownParser { .. }));
    assert!(err.to_string().contains("nope"));
}

#[test]
fn build_chain_reports_extends_errors_too() {
    let broken = registry_with(&[("child", Some("ghost"))]);
    assert!(matches!(
        broken.build_chain("child"),
        Err(RegistryError::UnknownExtends { .. })
    ));

    let cyclic = registry_with(&[("a", Some("b")), ("b", Some("a"))]);
    assert!(matches!(
        cyclic.build_chain("a"),
        Err(RegistryError::ExtendsCycle { .. })
    ));
}
