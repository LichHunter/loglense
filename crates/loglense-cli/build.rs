//! Embeds `LOGLENSE_VERSION` = `<crate version>+<short git rev>` for
//! `--version`. The rev comes from `LOGLENSE_GIT_REV` (set by the nix
//! build, where no `.git` exists), else `git rev-parse --short HEAD`,
//! else `dev`.

use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=LOGLENSE_GIT_REV");
    let git_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.git");
    // Re-stamp when HEAD moves (checkout) or the current branch advances.
    println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
    if let Ok(head) = std::fs::read_to_string(git_dir.join("HEAD")) {
        if let Some(reference) = head.trim().strip_prefix("ref: ") {
            println!(
                "cargo:rerun-if-changed={}",
                git_dir.join(reference).display()
            );
        }
    }
    let rev = std::env::var("LOGLENSE_GIT_REV")
        .ok()
        .filter(|rev| !rev.trim().is_empty())
        .or_else(git_rev)
        .unwrap_or_else(|| "dev".to_string());
    println!(
        "cargo:rustc-env=LOGLENSE_VERSION={}+{}",
        env!("CARGO_PKG_VERSION"),
        rev.trim()
    );
}

fn git_rev() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|rev| !rev.is_empty())
}
