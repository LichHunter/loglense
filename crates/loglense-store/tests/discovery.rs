//! TDD unit 1: state-directory discovery and creation.

use std::path::{Path, PathBuf};

use loglense_store::{create, discover, resolve, StoreError, STATE_DIR_NAME};

/// An unrelated home directory so the home boundary never interferes.
fn elsewhere_home() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir home")
}

fn make_state(parent: &Path) -> PathBuf {
    let state = parent.join(STATE_DIR_NAME);
    std::fs::create_dir_all(&state).expect("create state dir");
    state
}

#[test]
fn discovers_state_dir_from_nested_subdir() {
    let home = elsewhere_home();
    let proj = tempfile::tempdir().expect("tempdir proj");
    make_state(proj.path());
    let nested = proj.path().join("a/b/c");
    std::fs::create_dir_all(&nested).expect("mkdir nested");

    let found = discover(&nested, home.path()).expect("discover");

    assert_eq!(found, proj.path().join(STATE_DIR_NAME));
}

#[test]
fn discovery_stops_at_git_root_and_does_not_cross_it() {
    let home = elsewhere_home();
    let outer = tempfile::tempdir().expect("tempdir outer");
    make_state(outer.path()); // state ABOVE the git root — must not be found
    let repo = outer.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).expect("mkdir .git");
    std::fs::create_dir_all(repo.join("crates/inner")).expect("mkdir inner");

    let err = discover(&repo.join("crates/inner"), home.path()).unwrap_err();

    assert!(
        matches!(err, StoreError::StateDirNotFound { .. }),
        "expected StateDirNotFound, got {err:?}"
    );
}

#[test]
fn discovery_finds_state_dir_at_git_root_itself() {
    let home = elsewhere_home();
    let outer = tempfile::tempdir().expect("tempdir outer");
    let repo = outer.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).expect("mkdir .git");
    make_state(&repo);
    let sub = repo.join("deep/nested/dir");
    std::fs::create_dir_all(&sub).expect("mkdir sub");

    let found = discover(&sub, home.path()).expect("discover");

    assert_eq!(found, repo.join(STATE_DIR_NAME));
}

#[test]
fn discovery_stops_at_home_boundary() {
    let home = tempfile::tempdir().expect("tempdir home");
    let outer = tempfile::tempdir().expect("tempdir outer");
    make_state(outer.path()); // state ABOVE home — must not be found
    let proj = home.path().join("work/proj");
    std::fs::create_dir_all(&proj).expect("mkdir proj");

    let err = discover(&proj, home.path()).unwrap_err();

    assert!(
        matches!(err, StoreError::StateDirNotFound { .. }),
        "expected StateDirNotFound, got {err:?}"
    );
}

#[test]
fn discovery_finds_state_dir_in_home_itself() {
    let home = tempfile::tempdir().expect("tempdir home");
    make_state(home.path());
    let proj = home.path().join("work/proj");
    std::fs::create_dir_all(&proj).expect("mkdir proj");

    let found = discover(&proj, home.path()).expect("discover");

    assert_eq!(found, home.path().join(STATE_DIR_NAME));
}

#[test]
fn git_root_loses_to_home_when_home_is_shallower() {
    // home/work has the .git; .loglense sits between home and work — the
    // walk-up must stop at home before ever reaching it? No: home is the
    // shallower boundary, .loglense directly under home IS found.
    let home = tempfile::tempdir().expect("tempdir home");
    make_state(home.path());
    let repo = home.path().join("work/repo");
    std::fs::create_dir_all(repo.join(".git")).expect("mkdir .git");

    let found = discover(&repo, home.path()).expect("discover");

    assert_eq!(found, home.path().join(STATE_DIR_NAME));
}

#[test]
fn create_writes_self_gitignore_and_is_idempotent() {
    let home = elsewhere_home();
    let proj = tempfile::tempdir().expect("tempdir proj");

    let state = create(proj.path(), home.path(), false).expect("create");

    assert_eq!(state, proj.path().join(STATE_DIR_NAME));
    let gitignore = state.join(".gitignore");
    let content = std::fs::read_to_string(&gitignore).expect("read gitignore");
    assert_eq!(content.trim_end(), "*");

    // Second create leaves an existing .gitignore untouched.
    std::fs::write(&gitignore, "*\nruns/\n").expect("rewrite gitignore");
    create(proj.path(), home.path(), false).expect("create again");
    assert_eq!(
        std::fs::read_to_string(&gitignore).expect("reread"),
        "*\nruns/\n"
    );
}

#[test]
fn create_refuses_home_and_root_without_force() {
    let home = tempfile::tempdir().expect("tempdir home");

    let err = create(home.path(), home.path(), false).unwrap_err();
    assert!(
        matches!(err, StoreError::RefusedCreate { ref parent } if parent == home.path()),
        "expected RefusedCreate naming {home:?}, got {err:?}"
    );

    let root_err = create(Path::new("/"), home.path(), false).unwrap_err();
    assert!(
        matches!(root_err, StoreError::RefusedCreate { .. }),
        "{root_err:?}"
    );

    // --force overrides the home refusal.
    let state = create(home.path(), home.path(), true).expect("create with force");
    assert_eq!(state, home.path().join(STATE_DIR_NAME));
}

#[test]
fn state_dir_as_regular_file_is_typed_error() {
    let home = elsewhere_home();
    let proj = tempfile::tempdir().expect("tempdir proj");
    let file = proj.path().join(STATE_DIR_NAME);
    std::fs::write(&file, b"not a dir").expect("write file");

    let discover_err = discover(proj.path(), home.path()).unwrap_err();
    let create_err = create(proj.path(), home.path(), false).unwrap_err();
    assert!(
        matches!(discover_err, StoreError::StateDirIsFile { ref path } if path == &file),
        "{discover_err:?}"
    );
    assert!(
        matches!(create_err, StoreError::StateDirIsFile { ref path } if path == &file),
        "{create_err:?}"
    );
}

#[test]
fn symlinked_state_dir_is_followed_and_canonicalized() {
    let home = elsewhere_home();
    let proj = tempfile::tempdir().expect("tempdir proj");
    let real = tempfile::tempdir().expect("tempdir real");
    std::fs::create_dir_all(proj.path().join("deep")).expect("mkdir deep");
    std::os::unix::fs::symlink(real.path(), proj.path().join(STATE_DIR_NAME))
        .expect("symlink state dir");

    let found = discover(&proj.path().join("deep"), home.path()).expect("discover");

    assert_eq!(found, real.path().canonicalize().expect("canonical real"));
}

#[test]
fn resolve_falls_back_to_creation_when_no_state_found() {
    let home = elsewhere_home();
    let proj = tempfile::tempdir().expect("tempdir proj");

    let state = resolve(&proj.path().join("sub"), home.path()).expect("resolve");
    // Note: resolve creates in the START dir it was given.
    assert_eq!(state, proj.path().join("sub").join(STATE_DIR_NAME));
    assert!(state.join(".gitignore").is_file());
}
