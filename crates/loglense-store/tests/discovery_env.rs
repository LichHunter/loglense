//! TDD unit 1b: LOGLENSE_STATE_DIR env override short-circuits discovery.
//! Lives in its own test binary because it mutates process environment.

use std::sync::{Mutex, MutexGuard};

use loglense_store::resolve;

/// The two tests in this binary both mutate LOGLENSE_STATE_DIR, so they
/// must not run concurrently.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn env_override_short_circuits_discovery_and_creation() {
    let _guard = env_lock();
    let home = tempfile::tempdir().expect("tempdir home");
    let proj = tempfile::tempdir().expect("tempdir proj");
    // A decoy .loglense that discovery WOULD find if the env did not win.
    std::fs::create_dir_all(proj.path().join(".loglense")).expect("mkdir decoy");
    let env_dir = tempfile::tempdir().expect("tempdir env state");

    std::env::set_var("LOGLENSE_STATE_DIR", env_dir.path());
    let found = resolve(proj.path(), home.path()).expect("resolve");
    std::env::remove_var("LOGLENSE_STATE_DIR");

    assert_eq!(found, env_dir.path().canonicalize().expect("canonical"));
    assert_ne!(found, proj.path().join(".loglense"));

    // Without the env var, discovery finds the decoy again.
    let found = resolve(proj.path(), home.path()).expect("resolve without env");
    assert_eq!(found, proj.path().join(".loglense"));
}

#[test]
fn env_override_creates_missing_dir() {
    let _guard = env_lock();
    let home = tempfile::tempdir().expect("tempdir home");
    let start = tempfile::tempdir().expect("tempdir start");
    let env_dir = start.path().join("fresh/state");

    std::env::set_var("LOGLENSE_STATE_DIR", &env_dir);
    let found = resolve(start.path(), home.path()).expect("resolve");
    std::env::remove_var("LOGLENSE_STATE_DIR");

    assert!(found.is_dir());
    assert_eq!(found, env_dir.canonicalize().expect("canonical env dir"));
}
