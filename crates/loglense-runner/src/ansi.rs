//! ANSI escape-sequence stripping (docs/architecture.md §4.3): parsing works
//! on the stripped copy while the raw spools keep every byte pristine.

use std::sync::OnceLock;

use regex::Regex;

/// OSC strings (`ESC ]` … BEL / `ESC \`), CSI sequences (`ESC [` params,
/// intermediates, final byte), then two-character `ESC X` escapes. The
/// `regex` crate is linear-time, so hostile input cannot backtrack.
fn ansi_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b\[[0-?]*[ -/]*[@-~]|\x1b[@-_]")
            .unwrap_or_else(|_| unreachable!("static ANSI regex must compile"))
    })
}

/// Removes ANSI escape sequences from `line`; a lone trailing `ESC` stays.
pub(crate) fn strip(line: &str) -> String {
    if !line.contains('\x1b') {
        return line.to_string();
    }
    ansi_regex().replace_all(line, "").into_owned()
}

#[cfg(test)]
mod tests {
    use super::strip;

    #[test]
    fn strips_csi_color_sequences() {
        assert_eq!(strip("\x1b[31mERROR\x1b[0m boom"), "ERROR boom");
    }

    #[test]
    fn strips_two_char_escapes_and_plain_text_untouched() {
        assert_eq!(strip("\x1bMup and \x1b]down"), "up and down");
        assert_eq!(strip("plain line 42"), "plain line 42");
    }

    #[test]
    fn empty_and_bare_esc_stay_safe() {
        assert_eq!(strip(""), "");
        assert_eq!(strip("\x1b"), "\x1b");
    }
}
