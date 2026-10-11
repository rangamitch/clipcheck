//! Regression test for clipedit issue #32: the filename form of a time (used in
//! `suggest_output_name`) must agree with `format_hms` on the whole-second part.
//! Black-box: public API only. `format_hms_file` is private, so its output is
//! checked through `suggest_output_name`.

use clipcheck_clipedit::{format_hms, suggest_output_name};

/// `HH-MM-SS` from `format_hms`: the display form without fraction, colons replaced.
fn file_form_from_display(ms: i64) -> String {
    let display = format_hms(ms);
    let whole = display.split('.').next().expect("seconds part");
    whole.replace(':', "-")
}

#[test]
fn issue_32_suggested_range_agrees_with_format_hms_whole_seconds() {
    let cases: [(i64, i64); 5] = [
        (0, 12_204),
        (3_723_005, 3_723_999),
        (36_000_000 + 59_999, 360_000_000),
        (-5, 999),
        (1_000, 60_000),
    ];
    for (start, end) in cases {
        let name = suggest_output_name("Match.mp4", start, end);
        let expected = format!(
            "{}-{}",
            file_form_from_display(start),
            file_form_from_display(end)
        );
        assert!(
            name.contains(&expected),
            "start={start} end={end}: name `{name}` lacks `{expected}`"
        );
    }
}

#[test]
fn issue_32_range_has_no_colons_or_dots() {
    let name = suggest_output_name("Match.mp4", 3_723_005, 36_000_000);
    assert!(!name.contains(':'), "name: {name}");
    // The only dot allowed in the output is none: the fraction is dropped.
    assert!(!name.contains('.'), "name: {name}");
}
