//! Integration tests for [`Config`]: three-layer precedence (project over user
//! over built-in defaults), missing-file fallback, and error contracts.

use std::fs;
use std::path::Path;

use loglense_core::Config;

fn write_config(dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, content).expect("write config file");
    path
}

#[test]
fn missing_files_fall_back_to_builtin_defaults() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("project.toml");
    let user = tmp.path().join("user.toml");
    let config = Config::load(&project, &user).expect("missing files are not an error");
    assert_eq!(config, Config::default());
    assert_eq!(config.default_parser, "auto");
    assert_eq!(config.retention.max_age_days, 14);
    assert_eq!(config.retention.max_total_mb, 2048);
    assert_eq!(config.retention.min_free_mb, 100);
    assert_eq!(config.output.max_events, 50);
    assert_eq!(config.output.max_tail_lines, 100);
    assert_eq!(config.output.max_templates, 20);
}

#[test]
fn empty_file_is_equivalent_to_absent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = write_config(tmp.path(), "project.toml", "");
    let user = write_config(tmp.path(), "user.toml", "\n");
    let config = Config::load(&project, &user).expect("empty files parse to nothing");
    assert_eq!(config, Config::default());
}

#[test]
fn user_layer_overrides_defaults_field_by_field() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("project.toml"); // absent
    let user = write_config(
        tmp.path(),
        "user.toml",
        "[retention]\nmax_age_days = 7\n\n[output]\nmax_events = 99\n",
    );
    let config = Config::load(&project, &user).expect("user layer loads");
    assert_eq!(config.retention.max_age_days, 7, "user value wins");
    assert_eq!(config.retention.max_total_mb, 2048, "unset stays default");
    assert_eq!(config.output.max_events, 99);
    assert_eq!(config.default_parser, "auto");
}

#[test]
fn project_layer_wins_over_user_layer_and_defaults() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let user = write_config(
        tmp.path(),
        "user.toml",
        "default_parser = \"pytest\"\n\n[retention]\nmax_age_days = 7\n\n[output]\nmax_events = 99\n",
    );
    let project = write_config(
        tmp.path(),
        "project.toml",
        "default_parser = \"maven\"\n\n[output]\nmax_events = 10\nmax_templates = 3\n",
    );
    let config = Config::load(&project, &user).expect("three layers resolve");
    assert_eq!(config.default_parser, "maven", "project beats user");
    assert_eq!(
        config.retention.max_age_days, 7,
        "user survives where project is silent"
    );
    assert_eq!(config.output.max_events, 10, "project beats user");
    assert_eq!(config.output.max_templates, 3);
    assert_eq!(config.output.max_tail_lines, 100, "unset stays default");
}

#[test]
fn unknown_key_in_project_config_errors_naming_key_and_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let user = tmp.path().join("user.toml"); // absent
    let project = write_config(tmp.path(), "project.toml", "frobnicate = true\n");
    let err = Config::load(&project, &user).expect_err("unknown keys are rejected");
    let msg = err.to_string();
    eprintln!("probe -> {msg}");
    assert!(msg.contains("frobnicate"), "must name the key: {msg}");
    assert!(
        msg.contains(project.to_str().expect("utf8 path")),
        "must name the file: {msg}"
    );
}

#[test]
fn unknown_key_in_nested_section_errors_naming_key_and_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = write_config(tmp.path(), "project.toml", "[retention]\nwat = 1\n");
    let user = tmp.path().join("user.toml"); // absent
    let err = Config::load(&project, &user).expect_err("nested unknown keys are rejected");
    let msg = err.to_string();
    assert!(msg.contains("wat"), "must name the key: {msg}");
    assert!(
        msg.contains(project.to_str().expect("utf8 path")),
        "must name the file: {msg}"
    );
}

#[test]
fn wrong_type_errors_naming_field_and_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = write_config(
        tmp.path(),
        "project.toml",
        "[retention]\nmax_age_days = \"soon\"\n",
    );
    let user = tmp.path().join("user.toml"); // absent
    let err = Config::load(&project, &user).expect_err("type errors are rejected");
    let msg = err.to_string();
    assert!(msg.contains("max_age_days"), "must name the field: {msg}");
    assert!(
        msg.contains(project.to_str().expect("utf8 path")),
        "must name the file: {msg}"
    );
}

#[test]
fn broken_toml_syntax_errors_naming_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = write_config(tmp.path(), "project.toml", "not [valid toml\n");
    let user = tmp.path().join("user.toml"); // absent
    let err = Config::load(&project, &user).expect_err("broken syntax is rejected");
    assert!(
        err.to_string()
            .contains(project.to_str().expect("utf8 path")),
        "must name the file: {err}"
    );
}

#[test]
fn discover_uses_project_dot_loglense_and_home_user_config() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project_root = tmp.path().join("proj");
    let state = project_root.join(".loglense");
    fs::create_dir_all(&state).expect("create .loglense");
    write_config(&state, "config.toml", "[output]\nmax_events = 7\n");

    let home = tmp.path().join("home");
    let user_dir = home.join(".config").join("loglense");
    fs::create_dir_all(&user_dir).expect("create user config dir");
    write_config(
        &user_dir,
        "config.toml",
        "[output]\nmax_events = 70\nmax_templates = 4\n",
    );

    // Serialize HOME mutation: tests in one binary share the process env.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", &home);
    let config = Config::discover(&project_root);
    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }

    let config = config.expect("discover resolves both layers");
    assert_eq!(config.output.max_events, 7, "project layer wins");
    assert_eq!(config.output.max_templates, 4, "user layer survives");
}
