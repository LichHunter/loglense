//! Integration tests for template normalization and dedup grouping:
//! deterministic normalization, stable template ids, group accounting
//! (count/first/last/example), and the LRU cap.

use loglense_core::template::{normalize, template_id, TemplateGroups, DEFAULT_TEMPLATE_CAP};

#[test]
fn normalize_matches_documented_example() {
    assert_eq!(
        normalize("error at /a/b/c.rs:42 line 3 for 1000ms"),
        "error at P:N line N for Nms"
    );
}

#[test]
fn normalize_replaces_digit_runs_with_n() {
    assert_eq!(normalize("retry 3 times"), "retry N times");
    assert_eq!(normalize("42"), "N");
    assert_eq!(normalize("took 1000ms and 42s"), "took Nms and Ns");
    assert_eq!(normalize("no digits here"), "no digits here");
}

#[test]
fn normalize_replaces_long_hex_runs_with_h() {
    assert_eq!(normalize("hash 506f80de74ce end"), "hash H end");
    assert_eq!(normalize("addr 0xdeadbeef"), "addr 0xH");
    assert_eq!(normalize("commit DEADBEEFCAFEBABE"), "commit H");
    // Short hex-ish words are quantities/identifiers, not hashes.
    assert_eq!(normalize("code abc and abcd"), "code abc and abcd");
}

#[test]
fn normalize_replaces_double_quoted_strings_with_s() {
    assert_eq!(normalize("msg \"errno=42\" done"), "msg S done");
    assert_eq!(normalize("said \"hello\""), "said S");
    // Unterminated quotes are left alone (digit runs still apply).
    assert_eq!(normalize("oops \"open 7"), "oops \"open N");
}

#[test]
fn normalize_replaces_path_like_tokens_with_p() {
    assert_eq!(normalize("in src/main.rs at 9"), "in P at N");
    assert_eq!(normalize("bin /usr/bin/java"), "bin P");
    // Single separator, no dot: not a path.
    assert_eq!(normalize("a/b 3"), "a/b N");
    // Dot but no separator: not a path.
    assert_eq!(normalize("v1.2.3 3"), "vN.N.N N");
}

#[test]
fn normalize_handles_empty_and_pure_tokens() {
    assert_eq!(normalize(""), "");
    assert_eq!(normalize("   "), "");
    assert_eq!(normalize("P:N Nms S H"), "P:N Nms S H");
}

#[test]
fn normalize_is_idempotent() {
    for message in [
        "error at /a/b/c.rs:42 line 3 for 1000ms",
        "hash 506f80de74ce said \"x\"",
    ] {
        let once = normalize(message);
        assert_eq!(normalize(&once), once, "second pass must be stable: {once}");
    }
}

#[test]
fn template_id_is_twelve_hex_chars_and_stable() {
    let id = template_id("error at P:N line N for Nms");
    assert_eq!(id.len(), 12, "12-hex-char prefix: {id}");
    assert!(
        id.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "lowercase hex: {id}"
    );
    assert_eq!(template_id("error at P:N line N for Nms"), id);
    assert_ne!(template_id("other"), id);
}

#[test]
fn similar_lines_collapse_into_one_group_with_accounting() {
    let mut groups = TemplateGroups::new();
    let id1 = groups.record("error at /a/b/c.rs:1", 5);
    let id2 = groups.record("error at /a/b/c.rs:2", 9);
    let id3 = groups.record("error at /a/x.rs:3", 12);
    assert_eq!(id1, id2, "same normalized template");
    assert_eq!(id2, id3);
    assert_eq!(
        id1,
        template_id(&normalize("error at /a/b/c.rs:1")),
        "record computes normalize+id consistently"
    );
    assert_eq!(groups.len(), 1, "3 similar lines -> 1 template");
    let group = groups.get(&id1).expect("group exists");
    assert_eq!(group.count, 3);
    assert_eq!(group.first_seen, 5, "first occurrence line");
    assert_eq!(group.last_seen, 12, "last occurrence line");
    assert_eq!(
        group.example, "error at /a/b/c.rs:1",
        "example is the first original"
    );
}

#[test]
fn distinct_templates_form_distinct_groups() {
    let mut groups = TemplateGroups::new();
    groups.record("error boom", 1);
    groups.record("warn flaky", 2);
    assert_eq!(groups.len(), 2);
    assert!(groups.get(&template_id("error boom")).is_some());
    assert!(groups.get(&template_id("warn flaky")).is_some());
}

#[test]
fn lru_cap_evicts_oldest_when_full() {
    let mut groups = TemplateGroups::with_cap(3);
    let a = groups.record("a 1", 1);
    groups.record("b 2", 2);
    let c = groups.record("c 3", 3);
    groups.record("d 4", 4);
    assert_eq!(groups.len(), 3, "cap holds");
    assert!(groups.get(&a).is_none(), "oldest evicted");
    assert!(groups.get(&c).is_some());
    assert!(groups.get(&template_id(&normalize("d 4"))).is_some());
}

#[test]
fn lru_touch_revives_a_template() {
    let mut groups = TemplateGroups::with_cap(3);
    let a = groups.record("a 1", 1);
    groups.record("b 2", 2);
    groups.record("c 3", 3);
    groups.record("a 9", 4); // same template as `a`: "a N" — an LRU touch
    groups.record("d 4", 5); // over cap: b (untouched) must go
    assert!(groups.get(&a).is_some(), "touched template survives");
    assert!(
        groups.get(&template_id(&normalize("b 2"))).is_none(),
        "least recent evicted"
    );
}

/// Distinct lowercase word per number (digits alone normalize away).
fn word(i: usize) -> String {
    let mut s = String::new();
    let mut n = i;
    loop {
        s.push(char::from(b'a' + (n % 26) as u8));
        n /= 26;
        if n == 0 {
            break;
        }
    }
    s
}

#[test]
fn default_cap_is_ten_thousand_and_enforced() {
    assert_eq!(DEFAULT_TEMPLATE_CAP, 10_000);
    let mut groups = TemplateGroups::new();
    let first = groups.record("seed 0", 0);
    let mut last = first.clone();
    for i in 1..10_010 {
        last = groups.record(&format!("{} {}", word(i), i), i as u64);
    }
    assert!(
        groups.len() <= 10_000,
        "insert 10_010 -> capped: {}",
        groups.len()
    );
    assert!(
        groups.get(&first).is_none(),
        "oldest evicted at default cap"
    );
    assert!(groups.get(&last).is_some(), "newest survives");
}
