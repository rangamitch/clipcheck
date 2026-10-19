//! Tests for `clipcheck_ui_lib::app_cache` (black-box, public API only). Every test works in its
//! own TempDir; nothing touches the real %LOCALAPPDATA%\ClipCheck.

use std::fs;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use clipcheck_ui_lib::app_cache::{
    buffer_dir_from, clear_stale_buffer, prune_legacy_sidecars, stale_buffer_runs, AppCacheInfo,
    BUFFER_IDLE,
};

/// Age used for "old" files and folders: well past the 10 minute and 1 hour thresholds.
const OLD: Duration = Duration::from_secs(2 * 60 * 60);

/// Writes `bytes` bytes to `path`, creating parent folders.
fn make_file(path: &Path, bytes: usize) {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, vec![3u8; bytes]).expect("write file");
}

/// Sets the modification time of a file or folder to `age` before now.
///
/// Windows needs FILE_WRITE_ATTRIBUTES on the handle, and FILE_FLAG_BACKUP_SEMANTICS to open a
/// folder at all. Plain read access gives "Access is denied" there.
fn set_age(path: &Path, age: Duration) {
    let when = SystemTime::now() - age;
    let mut opts = File::options();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        opts.access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    }
    #[cfg(not(windows))]
    {
        opts.read(true);
    }
    opts.open(path)
        .expect("open for set_modified")
        .set_modified(when)
        .expect("set_modified");
}

/// Stamp folder `root/audio/<id>/<stamp>`.
fn stamp_dir(root: &Path, id: &str, stamp: &str) -> PathBuf {
    root.join("audio").join(id).join(stamp)
}

#[test]
fn legacy_m4a_is_deleted_and_v2_m4a_is_kept() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let dir = stamp_dir(root, "01REC100000000000000000000", "1234-5678");
    let legacy = dir.join("track0.m4a");
    let current = dir.join("track0.v2.m4a");
    make_file(&legacy, 100);
    make_file(&current, 100);

    let deleted = prune_legacy_sidecars(root);

    assert_eq!(deleted, 1);
    assert!(!legacy.exists(), "legacy sidecar must be deleted");
    assert!(current.is_file(), "v2 sidecar must be kept");
}

#[test]
fn legacy_m4a_is_deleted_regardless_of_age() {
    // A legacy sidecar is removed even when it was written a moment ago: the player never reuses it.
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let legacy = stamp_dir(root, "01REC100000000000000000000", "1-2").join("track1.m4a");
    make_file(&legacy, 10);

    assert_eq!(prune_legacy_sidecars(root), 1);
    assert!(!legacy.exists());
}

#[test]
fn old_part_file_is_deleted_and_fresh_part_file_is_kept() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let dir = stamp_dir(root, "01REC100000000000000000000", "1-2");
    let old_part = dir.join("track0.v2.part.m4a");
    let fresh_part = dir.join("track1.v2.part.m4a");
    make_file(&old_part, 40);
    make_file(&fresh_part, 40);
    set_age(&old_part, OLD);

    let deleted = prune_legacy_sidecars(root);

    assert_eq!(deleted, 1);
    assert!(
        !old_part.exists(),
        "a .part file older than an hour must be deleted"
    );
    assert!(
        fresh_part.is_file(),
        "a .part file younger than an hour must be kept"
    );
}

#[test]
fn part_file_without_m4a_suffix_follows_the_same_age_rule() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let dir = stamp_dir(root, "01REC100000000000000000000", "1-2");
    let old_part = dir.join("track0.part");
    let fresh_part = dir.join("track2.part");
    make_file(&old_part, 5);
    make_file(&fresh_part, 5);
    set_age(&old_part, OLD);

    assert_eq!(prune_legacy_sidecars(root), 1);
    assert!(!old_part.exists());
    assert!(fresh_part.is_file());
}

#[test]
fn other_files_in_stamp_folder_are_left_alone() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let dir = stamp_dir(root, "01REC100000000000000000000", "1-2");
    let notes = dir.join("notes.txt");
    let json = dir.join("meta.json");
    make_file(&notes, 4);
    make_file(&json, 4);
    set_age(&notes, OLD);
    set_age(&json, OLD);

    assert_eq!(prune_legacy_sidecars(root), 0);
    assert!(notes.is_file());
    assert!(json.is_file());
}

#[test]
fn non_audio_sibling_folders_are_untouched() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    // Legacy-looking files and old .part files outside root/audio/<id>/<stamp>.
    let thumb_m4a = root.join("thumbs").join("old.m4a");
    let peak_part = root.join("peaks").join("v2").join("old.part");
    let other_m4a = root.join("other").join("keep.m4a");
    let root_part = root.join("root.part");
    // A file directly under audio/ (no id or stamp folder) is outside the pruned layout too.
    let loose_m4a = root.join("audio").join("loose.m4a");
    // A file one level too deep (audio/<id>/<stamp>/sub/x.m4a) is not a stamp-level file.
    let deep_m4a = stamp_dir(root, "01REC100000000000000000000", "1-2")
        .join("sub")
        .join("deep.m4a");
    for p in [
        &thumb_m4a, &peak_part, &other_m4a, &root_part, &loose_m4a, &deep_m4a,
    ] {
        make_file(p, 9);
        set_age(p, OLD);
    }

    assert_eq!(prune_legacy_sidecars(root), 0);
    for p in [
        &thumb_m4a, &peak_part, &other_m4a, &root_part, &loose_m4a, &deep_m4a,
    ] {
        assert!(p.is_file(), "must be kept: {}", p.display());
    }
}

#[test]
fn empty_old_folders_under_audio_are_removed_but_audio_stays() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let audio = root.join("audio");
    let old_stamp = stamp_dir(root, "01REC100000000000000000000", "old");
    fs::create_dir_all(&old_stamp).expect("mkdir");
    set_age(&old_stamp, OLD);
    let old_id = audio.join("01REC100000000000000000000");
    set_age(&old_id, OLD);

    prune_legacy_sidecars(root);

    assert!(
        !old_stamp.exists(),
        "old empty stamp folder must be removed"
    );
    assert!(!old_id.exists(), "old empty id folder must be removed");
    assert!(audio.is_dir(), "the audio folder itself is never removed");
}

#[test]
fn empty_fresh_folders_under_audio_are_kept() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    // Created just now, so younger than the 10 minute grace period.
    let fresh_stamp = stamp_dir(root, "01REC100000000000000000000", "fresh");
    fs::create_dir_all(&fresh_stamp).expect("mkdir");

    prune_legacy_sidecars(root);

    assert!(
        fresh_stamp.is_dir(),
        "a fresh empty folder may be about to receive a file"
    );
}

#[test]
fn folder_emptied_by_this_prune_is_kept_until_it_is_old_enough() {
    // Deleting a legacy file bumps its folder's modification time to now, so the folder is still
    // inside the 10 minute grace period on this pass. It goes on a later pass, once it is old.
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let dir = stamp_dir(root, "01REC100000000000000000000", "1-2");
    make_file(&dir.join("track0.m4a"), 20);

    assert_eq!(prune_legacy_sidecars(root), 1);
    assert!(
        dir.is_dir(),
        "emptied folder is still fresh on the pass that emptied it"
    );

    set_age(&dir, OLD);
    prune_legacy_sidecars(root);
    assert!(
        !dir.exists(),
        "emptied folder is removed once it is old enough"
    );
}

#[test]
fn prune_on_missing_root_returns_zero() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().join("does-not-exist");

    assert_eq!(prune_legacy_sidecars(&missing), 0);
    assert!(!missing.exists(), "pruning must not create the root");
}

#[test]
fn prune_on_empty_root_returns_zero() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert_eq!(prune_legacy_sidecars(tmp.path()), 0);
}

#[cfg(unix)]
#[test]
fn symlinked_legacy_file_is_never_deleted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cache");
    let outside = tmp.path().join("outside.m4a");
    make_file(&outside, 30);
    let dir = stamp_dir(&root, "01REC100000000000000000000", "1-2");
    fs::create_dir_all(&dir).expect("mkdir");
    if std::os::unix::fs::symlink(&outside, dir.join("track0.m4a")).is_err() {
        return;
    }

    prune_legacy_sidecars(&root);

    assert!(outside.is_file(), "the link target must survive");
}

#[cfg(windows)]
#[test]
fn symlinked_folder_is_never_followed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cache");
    let outside = tmp.path().join("outside");
    make_file(&outside.join("track0.m4a"), 30);
    make_file(&outside.join("old.part"), 30);
    set_age(&outside.join("old.part"), OLD);
    fs::create_dir_all(root.join("audio")).expect("mkdir");
    if std::os::windows::fs::symlink_dir(&outside, root.join("audio").join("01LINK")).is_err() {
        // Creating symlinks needs developer mode or privileges on Windows; skip then.
        return;
    }

    assert_eq!(prune_legacy_sidecars(&root), 0);
    assert!(
        outside.join("track0.m4a").is_file(),
        "a linked folder is never followed"
    );
    assert!(
        outside.join("old.part").is_file(),
        "a linked folder is never followed"
    );
}

#[test]
fn app_cache_info_serialises_camel_case_keys() {
    let info = AppCacheInfo {
        dir: "D:\\cache".to_string(),
        player_bytes: 1,
        editor_bytes: 2,
        buffer_bytes: 4,
        total_bytes: 7,
    };
    let v = serde_json::to_value(&info).expect("json");
    let obj = v.as_object().expect("object");
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "bufferBytes",
            "dir",
            "editorBytes",
            "playerBytes",
            "totalBytes"
        ]
    );
    assert_eq!(v["playerBytes"], 1);
    assert_eq!(v["editorBytes"], 2);
    assert_eq!(v["bufferBytes"], 4);
    assert_eq!(v["totalBytes"], 7);
    assert_eq!(v["dir"], "D:\\cache");
}

// ---------- buffer_dir_from ----------

#[test]
fn buffer_dir_blank_or_missing_location_uses_default_folder() {
    let lad = "C:\\Users\\Tester\\AppData\\Local";
    let default = PathBuf::from(lad).join("ClipCheck").join("buffer");
    assert_eq!(buffer_dir_from(None, Some(lad)), default);
    assert_eq!(buffer_dir_from(Some(""), Some(lad)), default);
    assert_eq!(buffer_dir_from(Some("   "), Some(lad)), default);
}

#[test]
fn buffer_dir_without_local_app_data_uses_temp_dir() {
    let temp_default = std::env::temp_dir().join("ClipCheck").join("buffer");
    assert_eq!(buffer_dir_from(None, None), temp_default);
    assert_eq!(buffer_dir_from(Some("  "), None), temp_default);
}

#[test]
fn buffer_dir_literal_location_is_kept_and_trimmed() {
    let lad = "C:\\Users\\Tester\\AppData\\Local";
    assert_eq!(
        buffer_dir_from(Some("D:\\Replay\\buf"), Some(lad)),
        PathBuf::from("D:\\Replay\\buf")
    );
    assert_eq!(
        buffer_dir_from(Some("  D:\\Replay\\buf  "), Some(lad)),
        PathBuf::from("D:\\Replay\\buf")
    );
}

#[test]
fn buffer_dir_expands_localappdata_from_the_argument() {
    let lad = "C:\\Lad";
    assert_eq!(
        buffer_dir_from(Some("%LOCALAPPDATA%\\Alt"), Some(lad)),
        PathBuf::from("C:\\Lad\\Alt")
    );
    // Variable names match case-insensitively.
    assert_eq!(
        buffer_dir_from(Some("%localappdata%\\Alt"), Some(lad)),
        PathBuf::from("C:\\Lad\\Alt")
    );
}

#[test]
fn buffer_dir_keeps_unknown_variable_as_typed() {
    let out = buffer_dir_from(
        Some("%CLIPCHECK_TEST_NO_SUCH_VAR_7F3A%\\buf"),
        Some("C:\\Lad"),
    );
    assert_eq!(out, PathBuf::from("%CLIPCHECK_TEST_NO_SUCH_VAR_7F3A%\\buf"));
}

#[test]
fn buffer_idle_is_thirty_minutes() {
    assert_eq!(BUFFER_IDLE, Duration::from_secs(30 * 60));
}

// ---------- stale_buffer_runs / clear_stale_buffer ----------

/// Two hours after now: every run created by these tests is idle at this simulated time.
fn later() -> SystemTime {
    SystemTime::now() + Duration::from_secs(2 * 60 * 60)
}

/// Creates `buffer/<name>/stage-s1-p1/big.bin` (5000 bytes) and `buffer/<name>/meta.json`
/// (300 bytes). Total 5300 bytes. Returns the run folder.
fn make_run(buffer: &Path, name: &str) -> PathBuf {
    let run = buffer.join(name);
    make_file(&run.join("stage-s1-p1").join("big.bin"), 5000);
    make_file(&run.join("meta.json"), 300);
    run
}

/// Backdates a run's files and folders so the whole run is idle at the real current time.
fn age_run(run: &Path) {
    set_age(&run.join("stage-s1-p1").join("big.bin"), OLD);
    set_age(&run.join("meta.json"), OLD);
    set_age(&run.join("stage-s1-p1"), OLD);
    set_age(run, OLD);
}

const RUN_BYTES: u64 = 5300;

#[test]
fn stale_run_is_listed_with_its_byte_count() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let buffer = tmp.path().join("buffer");
    let run = make_run(&buffer, "run-1700000000000000000");

    let stale = stale_buffer_runs(&buffer, later());

    assert_eq!(stale, vec![(run, RUN_BYTES)]);
}

#[test]
fn fresh_run_at_real_time_is_not_listed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let buffer = tmp.path().join("buffer");
    make_run(&buffer, "run-1700000000000000001");

    assert!(stale_buffer_runs(&buffer, SystemTime::now()).is_empty());
}

#[test]
fn non_run_folders_and_plain_run_named_files_are_never_listed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let buffer = tmp.path().join("buffer");
    // A file whose name starts with run-.
    let plain = buffer.join("run-notes.txt");
    make_file(&plain, 4000);
    // A folder that is not a run.
    let other = buffer.join("keep").join("old.bin");
    make_file(&other, 700);
    // A run folder nested one level too deep.
    let nested = buffer.join("archive").join("run-9").join("big.bin");
    make_file(&nested, 800);
    for p in [&plain, &other, &nested] {
        set_age(p, OLD);
    }
    set_age(&buffer.join("keep"), OLD);
    set_age(&buffer.join("archive").join("run-9"), OLD);
    set_age(&buffer.join("archive"), OLD);

    assert!(stale_buffer_runs(&buffer, later()).is_empty());
    assert_eq!(clear_stale_buffer(&buffer, later()), 0);
    assert!(plain.is_file(), "run-named plain file must survive");
    assert!(other.is_file(), "non-run folder content must survive");
    assert!(nested.is_file(), "nested run-like folder must survive");
}

#[test]
fn clear_removes_only_stale_runs_and_returns_freed_bytes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let buffer = tmp.path().join("buffer");
    let old_run = make_run(&buffer, "run-1000");
    age_run(&old_run);
    let fresh_run = make_run(&buffer, "run-2000");
    let keep_folder_file = buffer.join("keep").join("x.bin");
    make_file(&keep_folder_file, 111);
    let plain = buffer.join("run-9.log");
    make_file(&plain, 222);

    let freed = clear_stale_buffer(&buffer, SystemTime::now());

    assert_eq!(freed, RUN_BYTES);
    assert!(!old_run.exists(), "stale run must be removed");
    assert!(fresh_run.join("meta.json").is_file(), "fresh run must stay");
    assert!(keep_folder_file.is_file(), "non-run folder must stay");
    assert!(plain.is_file(), "run-named plain file must stay");
}

#[test]
fn clear_then_info_reports_nothing_left_to_clear() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let buffer = tmp.path().join("buffer");
    let old_run = make_run(&buffer, "run-3000");
    age_run(&old_run);

    assert_eq!(clear_stale_buffer(&buffer, SystemTime::now()), RUN_BYTES);
    assert!(stale_buffer_runs(&buffer, later()).is_empty());
    assert_eq!(clear_stale_buffer(&buffer, later()), 0);
}

#[test]
fn missing_buffer_dir_gives_empty_and_zero_and_is_not_created() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().join("no-such-buffer");

    assert!(stale_buffer_runs(&missing, later()).is_empty());
    assert_eq!(clear_stale_buffer(&missing, later()), 0);
    assert!(!missing.exists(), "must not create the buffer folder");
}

/// Makes `link` a folder link to `target`. Returns false when links cannot be created here.
fn make_dir_link(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link).is_ok()
    }
}

#[test]
fn linked_run_folder_is_never_listed_or_followed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let buffer = tmp.path().join("buffer");
    fs::create_dir_all(&buffer).expect("mkdir");
    let outside = tmp.path().join("outside");
    let outside_file = outside.join("precious.bin");
    make_file(&outside_file, 900);
    if !make_dir_link(&outside, &buffer.join("run-link")) {
        return; // Links need privileges here; skip.
    }

    assert!(stale_buffer_runs(&buffer, later()).is_empty());
    assert_eq!(clear_stale_buffer(&buffer, later()), 0);
    assert!(outside_file.is_file(), "a linked run's target must survive");
}
