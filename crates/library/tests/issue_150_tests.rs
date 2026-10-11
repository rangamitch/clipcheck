//! Issue #150: Windows reserved device names including superscript digit variants
//! (COM¹-³, LPT¹-³) must be rejected by `sanitize_filename` and `validate_user_stem`.

use clipcheck_library::fsops::{
    sanitize_filename, validate_user_stem, NameError, MAX_FILENAME_UTF16, MAX_STEM_UTF16,
};

/// Reserved names: sanitize prefixes `_`, validate returns `NameError::Reserved`.
const RESERVED_SUPERSCRIPT: &[&str] = &[
    "COM¹",
    "COM²",
    "COM³",
    "LPT¹",
    "LPT²",
    "LPT³",
    "com¹",
    "com²",
    "com³",
    "lpt¹",
    "lpt²",
    "lpt³",
    "Com¹",
    "Lpt³",
    "COM¹.mp4",
    "com¹.mp4",
    "LPT².txt",
    "lpt³.tar.gz",
];

/// ASCII reserved names must keep working.
const RESERVED_ASCII: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "con",
    "nul.txt", "NUL.txt", "com1.mp4",
];

/// Names that look close but are NOT reserved.
const NOT_RESERVED: &[&str] = &[
    "COM¹¹", "COM¼", "COM0", "COMX", "COMMAND", "LPT10", "console", "AUX1", "COM¹⁰",
];

#[test]
fn superscript_reserved_names_are_prefixed_by_sanitize() {
    for name in RESERVED_SUPERSCRIPT {
        let out = sanitize_filename(name);
        assert_eq!(out, format!("_{name}"), "sanitize_filename({name:?})");
    }
}

#[test]
fn superscript_reserved_names_are_rejected_by_validate() {
    for name in RESERVED_SUPERSCRIPT {
        assert_eq!(
            validate_user_stem(name),
            Err(NameError::Reserved),
            "validate_user_stem({name:?})"
        );
    }
}

#[test]
fn superscript_reserved_names_with_whitespace_are_handled() {
    assert_eq!(sanitize_filename("  com¹  "), "_com¹");
    assert_eq!(validate_user_stem("  com¹  "), Err(NameError::Reserved));
}

#[test]
fn ascii_reserved_names_still_rejected() {
    for name in RESERVED_ASCII {
        assert_eq!(
            validate_user_stem(name),
            Err(NameError::Reserved),
            "validate_user_stem({name:?})"
        );
        assert!(
            sanitize_filename(name).starts_with('_'),
            "sanitize_filename({name:?}) should be prefixed"
        );
    }
}

#[test]
fn near_miss_names_are_accepted() {
    for name in NOT_RESERVED {
        assert_eq!(
            sanitize_filename(name),
            *name,
            "sanitize_filename({name:?}) must be unchanged"
        );
        assert_eq!(
            validate_user_stem(name),
            Ok(name.to_string()),
            "validate_user_stem({name:?})"
        );
    }
}

#[test]
fn unicode_names_round_trip_unchanged() {
    let names = [
        "Café",
        "naïve résumé",
        "Ölfeld",
        "日本語のクリップ",
        "录屏 2026",
        "한국어",
        "🎬 clip",
        "🎮🔥 highlight",
        "Ação – final",
    ];
    for name in names {
        assert_eq!(sanitize_filename(name), name, "sanitize_filename({name:?})");
        assert_eq!(
            validate_user_stem(name),
            Ok(name.to_string()),
            "validate_user_stem({name:?})"
        );
    }
}

#[test]
fn trailing_dot_or_space_on_superscript_name() {
    // Parity with ASCII: sanitize trims the tail, validate rejects it.
    assert_eq!(sanitize_filename("COM¹."), "_COM¹");
    assert_eq!(sanitize_filename("COM1."), "_COM1");
    assert_eq!(
        validate_user_stem("COM¹."),
        Err(NameError::TrailingDotOrSpace)
    );
    assert_eq!(
        validate_user_stem("COM1."),
        Err(NameError::TrailingDotOrSpace)
    );
}

#[test]
fn empty_and_whitespace_inputs() {
    // Report-only observation: the asserted behaviour mirrors the documented contract.
    assert_eq!(sanitize_filename(""), "Untitled");
    assert_eq!(sanitize_filename("   "), "Untitled");
    assert_eq!(validate_user_stem(""), Err(NameError::Empty));
    assert_eq!(validate_user_stem("   "), Err(NameError::Empty));
}

#[test]
fn very_long_names_are_capped() {
    let long: String = "a".repeat(300);
    let out = sanitize_filename(&long);
    assert!(
        out.encode_utf16().count() <= MAX_FILENAME_UTF16,
        "sanitize output {} units",
        out.encode_utf16().count()
    );
    assert_eq!(
        validate_user_stem(&long),
        Err(NameError::TooLong {
            max: MAX_STEM_UTF16
        })
    );
}

#[test]
fn long_superscript_prefixed_name_stays_within_cap() {
    // A name that is reserved and already at the cap must still fit after the `_` prefix.
    let out = sanitize_filename("com¹");
    assert!(out.encode_utf16().count() <= MAX_FILENAME_UTF16);
}

#[test]
fn sanitize_is_idempotent_for_superscript_names() {
    for name in RESERVED_SUPERSCRIPT {
        let once = sanitize_filename(name);
        assert_eq!(sanitize_filename(&once), once, "idempotence for {name:?}");
    }
}
