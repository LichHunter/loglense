//! Shared harness for the CLI integration suites: the real `loglense`
//! binary run in an isolated project (own HOME, fake git root, no
//! LOGLENSE_STATE_DIR).
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

pub const BIN: &str = env!("CARGO_BIN_EXE_loglense");

pub struct Project {
    _tmp: tempfile::TempDir,
    pub root: PathBuf,
    pub home: PathBuf,
}

impl Project {
    /// A git-root project dir (`.git/` present) with an isolated HOME.
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("proj");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        Self {
            _tmp: tmp,
            root,
            home,
        }
    }

    pub fn cmd(&self, args: &[&str]) -> Output {
        self.cmd_in(&self.root, args)
    }

    pub fn cmd_in(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.home)
            .env_remove("LOGLENSE_STATE_DIR")
            .output()
            .expect("loglense runs")
    }

    /// `--json` invocation that must exit `code`; returns parsed stdout.
    pub fn json(&self, args: &[&str], code: i32) -> Value {
        let mut full = vec!["--json"];
        full.extend_from_slice(args);
        let out = self.cmd(&full);
        assert_eq!(
            out.status.code(),
            Some(code),
            "{args:?}: {}",
            describe(&out)
        );
        serde_json::from_slice(&out.stdout).unwrap_or_else(|err| {
            panic!("{args:?}: stdout is not JSON ({err}): {}", describe(&out))
        })
    }

    /// Starts `cmd` via `run --json`; returns the run id.
    pub fn start(&self, cmd: &[&str]) -> String {
        let mut args = vec!["run", "--"];
        args.extend_from_slice(cmd);
        self.json(&args, 0)["run_id"]
            .as_str()
            .expect("run_id")
            .to_string()
    }

    /// Starts `cmd` and waits for it; returns the run id.
    pub fn finished(&self, cmd: &[&str]) -> String {
        let id = self.start(cmd);
        let out = self.cmd(&["wait", &id, "--timeout", "20"]);
        assert_ne!(out.status.code(), Some(124), "{}", describe(&out));
        id
    }
}

pub fn describe(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

pub fn assert_clean_failure(out: &Output, code: i32) {
    assert_eq!(out.status.code(), Some(code), "{}", describe(out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panicked"), "no panic: {stderr}");
    assert!(!stderr.trim().is_empty(), "an error message is printed");
}
