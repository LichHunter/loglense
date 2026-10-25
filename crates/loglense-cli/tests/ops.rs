//! Ops suites (T14): parser authoring (`parsers list|test|validate`) and the
//! feedback loop (`feedback leave|list|show|promote|resolve`) against real
//! wrapped runs.

use std::path::{Path, PathBuf};

use serde_json::Value;

mod common;
use common::{assert_clean_failure, describe, Project};

/// The docs/parsers.md §4.4 example pack (mytool extends java).
const MYTOOL: &str = r#"
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
"#;

fn write(path: &Path, text: &str) -> PathBuf {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, text).unwrap();
    path.to_path_buf()
}

#[test]
fn parsers_list_shows_builtins_project_packs_and_broken_packs() {
    let project = Project::new();
    project.json(&["init"], 0);
    write(
        &project.root.join(".loglense/formats.d/mytool.toml"),
        MYTOOL,
    );
    write(
        &project.root.join(".loglense/formats.d/broken.toml"),
        "schema = 1\nname =\n",
    );
    let list = project.json(&["parsers", "list"], 0);
    let rows = list["parsers"].as_array().unwrap();
    let find = |name: &str| rows.iter().find(|r| r["name"] == name).cloned();
    assert_eq!(find("cargo").unwrap()["source"], "built-in");
    assert_eq!(find("cargo").unwrap()["extends"], "rust");
    let mytool = find("mytool").expect("project pack listed");
    assert_eq!(mytool["source"], "project");
    assert_eq!(mytool["extends"], "java");
    assert_eq!(mytool["status"], "loaded");
    let broken = find("broken").expect("broken pack listed");
    assert!(broken["status"].as_str().unwrap().starts_with("broken:"));
    let human = String::from_utf8_lossy(&project.cmd(&["parsers", "list"]).stdout).into_owned();
    assert!(
        human.contains("mytool") && human.contains("broken:"),
        "{human}"
    );
}

#[test]
fn parsers_test_dry_runs_the_docs_example_pack_and_counts_unclaimed() {
    let project = Project::new();
    let pack = write(&project.root.join("mytool.toml"), MYTOOL);
    let sample = write(
        &project.root.join("sample.log"),
        "mytool [start]\n[ERR] disk full\nplain prose\nmytool: BUILD FAILED in 12s\n",
    );
    let out = project.json(
        &[
            "parsers",
            "test",
            pack.to_str().unwrap(),
            "--sample",
            sample.to_str().unwrap(),
        ],
        0,
    );
    assert_eq!(
        out["chain"],
        serde_json::json!(["mytool", "java", "generic"])
    );
    assert_eq!(out["lines"], 4);
    assert_eq!(out["unclaimed"], 2, "start banner + prose fall to generic");
    let events = out["events"].as_array().unwrap();
    assert!(events
        .iter()
        .any(|e| e["message"] == "disk full" && e["severity"] == "error"));
    assert!(events
        .iter()
        .any(|e| e["message"] == "build failed in 12 s" && e["kind"] == "summary"));
    let human = project.cmd(&[
        "parsers",
        "test",
        pack.to_str().unwrap(),
        "--sample",
        sample.to_str().unwrap(),
    ]);
    assert!(String::from_utf8_lossy(&human.stdout).contains("2 lines unclaimed"));
    // A registered parser's chain works without any pack.
    let cargo = project.json(
        &[
            "parsers",
            "test",
            "--parser",
            "cargo",
            "--sample",
            sample.to_str().unwrap(),
        ],
        0,
    );
    assert_eq!(cargo["chain"][0], "cargo");
}

#[test]
fn parsers_validate_is_clean_for_good_packs_and_lists_every_error() {
    let project = Project::new();
    let good = write(&project.root.join("good.toml"), MYTOOL);
    let ok = project.json(&["parsers", "validate", good.to_str().unwrap()], 0);
    assert_eq!(ok["valid"], true);
    let bad = write(
        &project.root.join("bad.toml"),
        "schema = 1\nname = \"x\"\n[[rule]]\nid = \"a\"\nseverity = \"error\"\npattern = \"[\"\nmessage = \"m\"\nkind = \"line\"\n[[rule]]\nid = \"b\"\nseverity = \"error\"\npattern = \"(\"\nmessage = \"m\"\nkind = \"line\"\n",
    );
    let dirty = project.json(&["parsers", "validate", bad.to_str().unwrap()], 2);
    assert_eq!(dirty["valid"], false);
    let errors: Vec<&str> = dirty["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap())
        .collect();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors[0].contains("rule[id=a].pattern") && errors[1].contains("rule[id=b].pattern"));
    assert_clean_failure(
        &project.cmd(&[
            "parsers",
            "test",
            bad.to_str().unwrap(),
            "--sample",
            good.to_str().unwrap(),
        ]),
        2,
    );
}

#[test]
fn feedback_round_trip_leave_list_show_resolve() {
    let project = Project::new();
    let run = project.finished(&["sh", "-c", "echo 'oops something broke'; exit 1"]);
    let left = project.json(
        &[
            "feedback",
            "leave",
            &run,
            "--kind",
            "parse-miss",
            "--detail",
            "line 1 is an error",
            "--line",
            "1",
            "--expected",
            "error",
        ],
        0,
    );
    let fid = left["feedback_id"].as_str().unwrap().to_string();
    assert!(fid.starts_with("f-"), "{fid}");

    let open = project.json(&["feedback", "list", "--status", "open"], 0);
    assert_eq!(open["total"], 1);
    assert_eq!(open["feedback"][0]["feedback_id"], fid.as_str());
    assert_eq!(open["feedback"][0]["run_id"], run.as_str());

    let shown = project.json(&["feedback", "show", &fid], 0);
    assert_eq!(shown["record"]["expected"], "error");
    assert_eq!(shown["record"]["parser"], "generic");
    assert_eq!(shown["record"]["line"], 1);

    let resolved = project.json(
        &[
            "feedback",
            "resolve",
            &fid,
            "--status",
            "fixed",
            "--note",
            "rule added",
        ],
        0,
    );
    assert_eq!(resolved["record"]["status"], "fixed");
    assert_eq!(resolved["record"]["note"], "rule added");
    assert_eq!(
        project.json(&["feedback", "list", "--status", "open"], 0)["total"],
        0
    );
    assert_eq!(
        project.json(&["feedback", "list", "--status", "fixed"], 0)["total"],
        1
    );
    let global = project.json(&["feedback", "list", "--global"], 0);
    assert_eq!(
        global["feedback"][0]["status"], "fixed",
        "index pointer rewritten"
    );
    assert_clean_failure(&project.cmd(&["feedback", "show", "f-0-0"]), 2);
}

#[test]
fn feedback_promote_writes_a_failing_fixture_window_into_the_repo() {
    let project = Project::new();
    let script = "i=1; while [ $i -le 60 ]; do echo \"line $i\"; i=$((i+1)); done";
    let run = project.finished(&["sh", "-c", script]);
    let left = project.json(
        &[
            "feedback",
            "leave",
            &run,
            "--kind",
            "parse-miss",
            "--detail",
            "d",
            "--line",
            "30",
        ],
        0,
    );
    let fid = left["feedback_id"].as_str().unwrap().to_string();
    let repo = project.root.parent().unwrap().join("loglense-checkout");
    std::fs::create_dir_all(repo.join("crates/loglense-parsers/tests/fixtures")).unwrap();

    let promoted = project.json(
        &[
            "feedback",
            "promote",
            &fid,
            "--repo",
            repo.to_str().unwrap(),
        ],
        0,
    );
    assert_eq!(promoted["first_line"], 10);
    assert_eq!(promoted["last_line"], 50);
    assert_eq!(promoted["stream"], "out");
    assert_eq!(
        promoted["command"],
        "cargo test -p loglense-parsers --test golden"
    );
    let dir = repo.join("crates/loglense-parsers/tests/fixtures/generic");
    let log = std::fs::read_to_string(dir.join(format!("{fid}.log"))).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 41);
    assert_eq!(lines[0], "line 10");
    assert_eq!(lines[40], "line 50");
    assert_eq!(
        std::fs::read_to_string(dir.join(format!("{fid}.expected.json"))).unwrap(),
        "[]\n",
        "fails until a maintainer fills it"
    );
    // Never overwrites without --force.
    assert_clean_failure(
        &project.cmd(&[
            "feedback",
            "promote",
            &fid,
            "--repo",
            repo.to_str().unwrap(),
        ]),
        2,
    );
    let forced = project.cmd(&[
        "feedback",
        "promote",
        &fid,
        "--repo",
        repo.to_str().unwrap(),
        "--force",
    ]);
    assert_eq!(forced.status.code(), Some(0), "{}", describe(&forced));
}

#[test]
fn promote_with_deleted_raws_and_outside_a_checkout_fails_cleanly() {
    let project = Project::new();
    let run = project.finished(&["sh", "-c", "echo hi"]);
    let fid = project.json(
        &[
            "feedback", "leave", &run, "--kind", "bug", "--detail", "d", "--line", "1",
        ],
        0,
    )["feedback_id"]
        .as_str()
        .unwrap()
        .to_string();
    let not_repo = project.cmd(&[
        "feedback",
        "promote",
        &fid,
        "--repo",
        project.home.to_str().unwrap(),
    ]);
    assert_clean_failure(&not_repo, 2);
    assert!(String::from_utf8_lossy(&not_repo.stderr).contains("--repo"));

    let repo = project.root.parent().unwrap().join("checkout");
    std::fs::create_dir_all(repo.join("crates/loglense-parsers/tests/fixtures")).unwrap();
    let run_dir = project.root.join(".loglense/runs").join(&run);
    std::fs::remove_file(run_dir.join("stdout.raw")).unwrap();
    let missing = project.cmd(&[
        "feedback",
        "promote",
        &fid,
        "--repo",
        repo.to_str().unwrap(),
        "--stream",
        "out",
    ]);
    assert_clean_failure(&missing, 2);
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("stdout.raw"),
        "names the missing file: {}",
        describe(&missing)
    );
}

#[test]
fn global_list_marks_dangling_pointers_as_orphans() {
    let project = Project::new();
    let run = project.finished(&["true"]);
    let fid = project.json(
        &[
            "feedback", "leave", &run, "--kind", "other", "--detail", "d",
        ],
        0,
    )["feedback_id"]
        .as_str()
        .unwrap()
        .to_string();
    std::fs::remove_file(
        project
            .root
            .join(".loglense/feedback")
            .join(format!("{fid}.json")),
    )
    .unwrap();
    let global = project.json(&["feedback", "list", "--global"], 0);
    let row: &Value = &global["feedback"][0];
    assert_eq!(row["feedback_id"], fid.as_str());
    assert_eq!(row["orphan"], true);
    let human = String::from_utf8_lossy(&project.cmd(&["feedback", "list", "--global"]).stdout)
        .into_owned();
    assert!(human.contains("ORPHAN"), "{human}");
}
