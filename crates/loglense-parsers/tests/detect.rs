//! Autodetection (docs/parsers.md §3.3): every registered module's `detect`
//! runs over the buffered sample; the highest score above 0 wins, ties break
//! to registration order, and `generic` is the permanent fallback.

mod common;

use common::MockParser;
use loglense_parsers::{detect_from_sample, ParserRegistry, Source};

fn registry_with_scores(scores: &[(&'static str, u8)]) -> ParserRegistry {
    let mut registry = ParserRegistry::new();
    for (name, score) in scores {
        registry.register(
            name,
            MockParser::new(name).with_score(*score).ctor(),
            Source::Builtin,
            None,
        );
    }
    registry
}

#[test]
fn highest_score_wins() {
    let registry = registry_with_scores(&[("a", 10), ("b", 30), ("c", 5)]);
    assert_eq!(detect_from_sample(&registry, &["line"]), "b");
}

#[test]
fn tie_breaks_to_registration_order() {
    let first_registered = registry_with_scores(&[("a", 30), ("b", 30)]);
    assert_eq!(detect_from_sample(&first_registered, &["line"]), "a");

    let reversed = registry_with_scores(&[("b", 30), ("a", 30)]);
    assert_eq!(detect_from_sample(&reversed, &["line"]), "b");
}

#[test]
fn all_zero_scores_fall_back_to_generic() {
    let registry = registry_with_scores(&[("a", 0), ("b", 0)]);
    assert_eq!(detect_from_sample(&registry, &["line"]), "generic");
}

#[test]
fn empty_registry_falls_back_to_generic() {
    let registry = ParserRegistry::new();
    assert_eq!(detect_from_sample(&registry, &[]), "generic");
}

#[test]
fn empty_sample_falls_back_to_generic() {
    let registry = registry_with_scores(&[("a", 90)]);
    assert_eq!(detect_from_sample(&registry, &[]), "generic");
}

#[test]
fn packs_after_builtins_in_registration_order() {
    let mut registry = ParserRegistry::new();
    for (name, score, source) in [
        ("builtin-a", 40, Source::Builtin),
        ("pack-z", 40, Source::UserPack),
        ("pack-y", 40, Source::ProjectPack),
    ] {
        registry.register(
            name,
            MockParser::new(name).with_score(score).ctor(),
            source,
            None,
        );
    }
    assert_eq!(
        detect_from_sample(&registry, &["line"]),
        "builtin-a",
        "registration order breaks the tie: built-ins registered first"
    );
}
