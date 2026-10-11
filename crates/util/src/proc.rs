//! Child-process helpers (DESIGN.md D51). No dependencies, no unsafe.

/// Win32 `CREATE_NO_WINDOW` process creation flag: the child gets no console window.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Maximum number of non-empty stderr lines kept by [`stderr_brief`].
pub const BRIEF_LINES: usize = 4;

/// Maximum byte length of the string returned by [`stderr_brief`].
pub const BRIEF_MAX_BYTES: usize = 500;

/// Hides the console window of a child process on Windows.
///
/// On Windows this sets `CREATE_NO_WINDOW` via `creation_flags`, replacing any
/// creation flags set earlier on the same command. Callers that need extra
/// flags (for example `CREATE_NEW_PROCESS_GROUP`) must use [`CREATE_NO_WINDOW`]
/// together with those flags directly through `creation_flags`.
/// On other platforms the command is returned unchanged.
#[cfg(windows)]
pub fn no_window(cmd: &mut std::process::Command) -> &mut std::process::Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(CREATE_NO_WINDOW)
}

/// Hides the console window of a child process on Windows.
///
/// On non-Windows platforms this is a no-op that returns the command unchanged.
#[cfg(not(windows))]
pub fn no_window(cmd: &mut std::process::Command) -> &mut std::process::Command {
    cmd
}

/// Returns the trailing part of a child's stderr, at most `max_bytes` bytes.
///
/// The bytes are decoded lossily and trimmed. If the result is longer than
/// `max_bytes`, the last `max_bytes` bytes are kept, with the cut moved forward
/// to the next char boundary so the result is valid UTF-8 and at most
/// `max_bytes` long. Newlines inside the text are kept. `max_bytes == 0`
/// yields an empty string.
pub fn stderr_tail(bytes: &[u8], max_bytes: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.len() <= max_bytes {
        return trimmed.to_string();
    }
    let mut start = trimmed.len() - max_bytes;
    while !trimmed.is_char_boundary(start) {
        start += 1;
    }
    trimmed[start..].to_string()
}

/// Returns a one-line summary of a child's stderr.
///
/// Takes the last [`BRIEF_LINES`] non-empty lines (each trimmed), joins them
/// with `" | "`, and truncates the result to [`BRIEF_MAX_BYTES`] at the
/// largest char boundary not above that limit, keeping the start. Blank or
/// empty input yields an empty string.
pub fn stderr_brief(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let skip = lines.len().saturating_sub(BRIEF_LINES);
    let mut joined = lines[skip..].join(" | ");
    if joined.len() > BRIEF_MAX_BYTES {
        let mut cut = BRIEF_MAX_BYTES;
        while !joined.is_char_boundary(cut) {
            cut -= 1;
        }
        joined.truncate(cut);
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn tail_cut_moves_forward_to_char_boundary() {
        // "ab" (2) + "€" (3 bytes: E2 82 AC) + "cd" (2) = 7 bytes.
        // Keeping the last 4 bytes starts inside "€", so the cut moves to "c".
        assert_eq!(stderr_tail("ab€cd".as_bytes(), 4), "cd");
        // Keeping the last 5 bytes starts exactly at "€".
        assert_eq!(stderr_tail("ab€cd".as_bytes(), 5), "€cd");
        assert!(stderr_tail("ab€cd".as_bytes(), 4).len() <= 4);
    }

    #[test]
    fn tail_max_zero_is_empty() {
        assert_eq!(stderr_tail(b"some error", 0), "");
        assert_eq!(stderr_tail(b"", 0), "");
    }

    #[test]
    fn tail_short_input_is_trimmed_and_kept() {
        assert_eq!(stderr_tail(b"  boom\nline2 \n", 500), "boom\nline2");
        assert_eq!(stderr_tail(b"   \n ", 10), "");
    }

    #[test]
    fn tail_invalid_utf8_does_not_panic() {
        let out = stderr_tail(&[0xff, b'a', b'b', 0xfe], 2);
        assert!(out.len() <= 2);
    }

    #[test]
    fn brief_keeps_last_four_non_empty_lines_joined() {
        let input = b"l1\n\n  l2  \nl3\n   \nl4\nl5\nl6\n";
        assert_eq!(stderr_brief(input), "l3 | l4 | l5 | l6");
    }

    #[test]
    fn brief_fewer_lines_all_kept() {
        assert_eq!(stderr_brief(b"only one\n"), "only one");
    }

    #[test]
    fn brief_blank_input_is_empty() {
        assert_eq!(stderr_brief(b""), "");
        assert_eq!(stderr_brief(b" \n\r\n\t\n"), "");
    }

    #[test]
    fn brief_cap_cuts_on_char_boundary() {
        // 200 "€" = 600 bytes. Largest boundary <= 500 is 498 (166 chars).
        let input = "€".repeat(200);
        let out = stderr_brief(input.as_bytes());
        assert_eq!(out.len(), 498);
        assert!(out.len() <= BRIEF_MAX_BYTES);
        assert_eq!(out.chars().count(), 166);
        assert!(out.chars().all(|c| c == '€'));
    }

    #[test]
    fn no_window_returns_command() {
        let mut cmd = Command::new("cmd");
        let _ = no_window(&mut cmd);
    }
}
