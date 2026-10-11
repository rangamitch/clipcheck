use std::process::Command;
use std::sync::{Arc, Mutex};

use clipcheck_util::proc::{
    no_window, stderr_brief, stderr_tail, BRIEF_LINES, BRIEF_MAX_BYTES, CREATE_NO_WINDOW,
};
use clipcheck_util::sync::lock_unpoisoned;

#[test]
fn constants_have_documented_values() {
    assert_eq!(CREATE_NO_WINDOW, 0x0800_0000);
    assert_eq!(BRIEF_LINES, 4);
    assert_eq!(BRIEF_MAX_BYTES, 500);
}

#[test]
fn tail_short_input_is_trimmed() {
    assert_eq!(stderr_tail(b"  hello \n", 100), "hello");
}

#[test]
fn tail_ascii_keeps_last_max_bytes() {
    assert_eq!(stderr_tail(b"abcdefghij", 4), "ghij");
    assert_eq!(stderr_tail(b"abcdefghij", 10), "abcdefghij");
    assert_eq!(stderr_tail(b"abcdefghij", 20), "abcdefghij");
}

#[test]
fn tail_multibyte_straddling_cut_stays_valid_and_within_max() {
    let s = "aé€😀";
    assert_eq!(s.len(), 10);
    for max in 0..=s.len() {
        let out = stderr_tail(s.as_bytes(), max);
        assert!(
            out.len() <= max,
            "max={max} got {out:?} ({} bytes)",
            out.len()
        );
        assert!(
            s.ends_with(out.as_str()),
            "max={max} result {out:?} is not a suffix of input"
        );
    }
}

#[test]
fn tail_max_zero_is_empty() {
    assert_eq!(stderr_tail(b"anything at all", 0), "");
    assert_eq!(stderr_tail(b"", 0), "");
}

#[test]
fn tail_invalid_utf8_does_not_panic_and_is_valid() {
    let bytes = [0xFFu8, 0xFE, b'o', b'k'];
    let full = stderr_tail(&bytes, 100);
    assert!(full.contains("ok"), "got {full:?}");
    for max in 0..=full.len() + 2 {
        let out = stderr_tail(&bytes, max);
        assert!(out.len() <= max, "max={max} got {out:?}");
    }
}

#[test]
fn tail_keeps_inner_newlines() {
    assert_eq!(
        stderr_tail(b"line1\nline2\nline3", 100),
        "line1\nline2\nline3"
    );
    assert_eq!(stderr_tail(b"  l1\nl2  \n", 100), "l1\nl2");
}

#[test]
fn brief_skips_blank_lines() {
    assert_eq!(stderr_brief(b"a\n\n   \nb\n\t\nc"), "a | b | c");
}

#[test]
fn brief_keeps_only_last_four_lines() {
    assert_eq!(
        stderr_brief(b"l1\nl2\nl3\nl4\nl5\nl6\n"),
        "l3 | l4 | l5 | l6"
    );
}

#[test]
fn brief_caps_at_500_bytes_on_char_boundary() {
    // 300 * "é" (2 bytes each) = 600 bytes on one line.
    let input: String = "é".repeat(300);
    let out = stderr_brief(input.as_bytes());
    assert!(out.len() <= 500, "len={}", out.len());
    assert!(!out.is_empty());
    assert!(
        input.starts_with(out.as_str()),
        "result must keep the start"
    );
    // 500 is an even boundary for 2-byte chars, so exactly 250 chars fit.
    assert_eq!(out.len(), 500);
}

#[test]
fn brief_multibyte_on_500_boundary_cuts_before_it() {
    // 499 'a' bytes then a 3-byte '€' occupying bytes 499..502.
    // Largest char boundary <= 500 is 499, so the result is the 499 'a's.
    let mut input = "a".repeat(499);
    input.push('€');
    assert_eq!(input.len(), 502);
    let out = stderr_brief(input.as_bytes());
    assert_eq!(out, "a".repeat(499));
}

#[test]
fn brief_empty_input_is_empty() {
    assert_eq!(stderr_brief(b""), "");
}

#[test]
fn brief_whitespace_only_input_is_empty() {
    assert_eq!(stderr_brief(b"  \n\t\n \r\n"), "");
}

#[test]
fn lock_unpoisoned_healthy_mutex() {
    let m = Mutex::new(vec![1]);
    {
        let mut g = lock_unpoisoned(&m);
        g.push(2);
    }
    assert_eq!(*lock_unpoisoned(&m), vec![1, 2]);
}

#[test]
fn lock_unpoisoned_recovers_poisoned_mutex() {
    let m = Arc::new(Mutex::new(5i32));
    let m2 = Arc::clone(&m);
    let res = std::thread::spawn(move || {
        let _g = m2.lock().unwrap();
        panic!("intentional poison");
    })
    .join();
    assert!(res.is_err(), "thread should have panicked");
    assert!(m.is_poisoned(), "mutex should be poisoned");

    {
        let mut g = lock_unpoisoned(&m);
        assert_eq!(*g, 5);
        *g = 7;
    }
    assert_eq!(*lock_unpoisoned(&m), 7);
}

#[cfg(windows)]
#[test]
fn no_window_returns_same_command_and_spawns() {
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", "exit 0"]);
    let input: *mut Command = &mut cmd;
    let returned = no_window(&mut cmd);
    assert!(
        std::ptr::eq(returned, input),
        "must return the same Command"
    );
    let out = returned.output().expect("cmd /C exit 0 should spawn");
    assert!(out.status.success(), "status={:?}", out.status);
}

#[cfg(not(windows))]
#[test]
fn no_window_returns_same_command_and_spawns() {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "exit 0"]);
    let input: *mut Command = &mut cmd;
    let returned = no_window(&mut cmd);
    assert!(
        std::ptr::eq(returned, input),
        "must return the same Command"
    );
    let out = returned.output().expect("sh -c exit 0 should spawn");
    assert!(out.status.success(), "status={:?}", out.status);
}
