//! TDD unit 7: adversarial probes — permission errors name paths.

use std::os::unix::fs::{MetadataExt, PermissionsExt};

use loglense_store::{create, discover, StoreError};

fn euid() -> u32 {
    // /proc is Linux-only by design (store targets Linux).
    std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .unwrap_or(0)
}

#[test]
fn chmod_000_dir_yields_eacces_error_naming_path() {
    if euid() == 0 {
        eprintln!("SKIP: running as root, chmod 000 does not deny access");
        return;
    }
    let home = tempfile::tempdir().expect("tempdir home");
    let outer = tempfile::tempdir().expect("tempdir outer");
    let proj = outer.path().join("locked/proj");
    std::fs::create_dir_all(&proj).expect("mkdir proj");
    std::fs::set_permissions(
        outer.path().join("locked"),
        std::fs::Permissions::from_mode(0o000),
    )
    .expect("chmod 000");

    let create_err = create(&proj, home.path(), false).unwrap_err();
    let message = create_err.to_string();
    assert!(
        message.contains("locked/proj") && message.to_lowercase().contains("permission denied"),
        "EACCES error must name the path, got: {message}"
    );

    let discover_err = discover(&proj, home.path()).unwrap_err();
    // Discovery inside the unreadable tree: walking up fails to stat
    // candidates, so it reports not-found or EACCES — but never panics.
    assert!(
        matches!(
            discover_err,
            StoreError::StateDirNotFound { .. } | StoreError::Io { .. }
        ),
        "{discover_err:?}"
    );

    std::fs::set_permissions(
        outer.path().join("locked"),
        std::fs::Permissions::from_mode(0o700),
    )
    .expect("chmod restore");
}
