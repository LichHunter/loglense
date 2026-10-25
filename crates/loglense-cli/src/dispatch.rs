//! Time-style prefix dispatch (docs/surfaces.md §1.2): when the first
//! non-flag argument is not a reserved subcommand, argv is rewritten to
//! `run [run-flags] -- <cmd...>` so nothing in the wrapped command is ever
//! parsed as a loglense flag.

/// First arguments that name a subcommand rather than a command to wrap.
pub(crate) const RESERVED: [&str; 16] = [
    "run", "init", "ls", "show", "errors", "warnings", "tail", "wait", "kill", "gc", "parsers",
    "feedback", "notify", "mcp", "help", "version",
];

/// Global flags taking a value (may precede the command in prefix form).
const GLOBAL_VALUE_FLAGS: [&str; 1] = ["--dir"];
/// Global boolean flags.
const GLOBAL_BOOL_FLAGS: [&str; 1] = ["--json"];
/// `run` flags taking a value that the prefix form also accepts.
const RUN_VALUE_FLAGS: [&str; 3] = ["--parser", "--on-complete", "--on-complete-timeout"];

/// Rewrites `argv` for the prefix form; anything else passes through
/// unchanged for clap to parse (or reject).
pub(crate) fn rewrite(args: Vec<String>) -> Vec<String> {
    let mut globals = Vec::new();
    let mut run_flags = Vec::new();
    let mut i = 1;
    while let Some(arg) = args.get(i) {
        let name = arg.split_once('=').map_or(arg.as_str(), |(name, _)| name);
        let inline_value = arg.contains('=');
        let target = if GLOBAL_VALUE_FLAGS.contains(&name) || GLOBAL_BOOL_FLAGS.contains(&name) {
            &mut globals
        } else if RUN_VALUE_FLAGS.contains(&name) {
            &mut run_flags
        } else {
            break;
        };
        target.push(arg.clone());
        i += 1;
        let takes_value = !GLOBAL_BOOL_FLAGS.contains(&name) && !inline_value;
        if takes_value {
            match args.get(i) {
                Some(value) => target.push(value.clone()),
                None => return args,
            }
            i += 1;
        }
    }
    let Some(first) = args.get(i) else {
        return args;
    };
    let is_command =
        first == "--" || !(first.starts_with('-') || RESERVED.contains(&first.as_str()));
    if !is_command {
        return args;
    }
    let skip = usize::from(first == "--");
    let mut out = Vec::with_capacity(args.len() + 2);
    out.push(args[0].clone());
    out.extend(globals);
    out.push("run".to_string());
    out.extend(run_flags);
    out.push("--".to_string());
    out.extend(args[i + skip..].iter().cloned());
    out
}

#[cfg(test)]
mod tests {
    use super::rewrite;

    fn argv(args: &[&str]) -> Vec<String> {
        std::iter::once("loglense")
            .chain(args.iter().copied())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn bare_command_becomes_explicit_run() {
        assert_eq!(
            rewrite(argv(&["sh", "-c", "echo --json"])),
            argv(&["run", "--", "sh", "-c", "echo --json"])
        );
    }

    #[test]
    fn leading_double_dash_is_the_prefix_form() {
        assert_eq!(
            rewrite(argv(&["--", "ls", "-la"])),
            argv(&["run", "--", "ls", "-la"]),
            "after `--` even a reserved word is the command"
        );
    }

    #[test]
    fn global_and_run_flags_before_the_command_are_kept() {
        assert_eq!(
            rewrite(argv(&[
                "--dir",
                "/s",
                "--json",
                "--parser=cargo",
                "cargo",
                "build"
            ])),
            argv(&[
                "--dir",
                "/s",
                "--json",
                "run",
                "--parser=cargo",
                "--",
                "cargo",
                "build"
            ])
        );
    }

    #[test]
    fn reserved_words_and_flags_pass_through() {
        for args in [
            &["ls", "--max", "3"][..],
            &["--json", "show", "x"][..],
            &["--version"][..],
            &["--bogus"][..],
            &[][..],
            &["--dir"][..],
        ] {
            assert_eq!(rewrite(argv(args)), argv(args), "{args:?}");
        }
    }
}
