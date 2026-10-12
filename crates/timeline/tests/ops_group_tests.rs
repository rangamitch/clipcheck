//! Tester tests for `ops_group`: selection expansion, groups, A/V links, detach audio, group
//! edits and group gain. Written from the doc comments in `src/ops_group.rs` (public API only).
//!
//! Synthetic data only. Clips are built directly with explicit track ids; no media is needed
//! because these operations do not check source ranges. Helpers are inline on purpose.

use clipcheck_timeline::error::EditError;
use clipcheck_timeline::model::{Clip, ClipKind, Project};
use clipcheck_timeline::ops_group::{
    adjust_group_gain, detach_audio, edit_group, expand_selection, group_clips, link_clips,
    ungroup, unlink_clips,
};

fn project() -> Project {
    Project::new("group-tests")
}

fn tid(p: &Project, name: &str) -> String {
    p.tracks
        .iter()
        .find(|t| t.name == name)
        .map(|t| t.id.clone())
        .expect("track exists")
}

fn clip(p: &Project, id: &str, track: &str, kind: ClipKind) -> Clip {
    Clip {
        id: id.to_string(),
        track_id: tid(p, track),
        kind,
        ..Clip::default()
    }
}

fn vid(p: &Project, id: &str) -> Clip {
    clip(p, id, "V1", ClipKind::Video)
}

fn aud(p: &Project, id: &str) -> Clip {
    clip(p, id, "A1", ClipKind::Audio)
}

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn get(p: &Project, id: &str) -> Clip {
    p.clip(id).cloned().expect("clip exists")
}

fn group_of(p: &Project, id: &str) -> Option<String> {
    p.clip(id).and_then(|c| c.group_id.clone())
}

fn link_of(p: &Project, id: &str) -> Option<String> {
    p.clip(id).and_then(|c| c.link_id.clone())
}

fn in_group(mut c: Clip, group: &str) -> Clip {
    c.group_id = Some(group.to_string());
    c
}

fn in_link(mut c: Clip, link: &str) -> Clip {
    c.link_id = Some(link.to_string());
    c
}

// ---------------------------------------------------------------------------------------------
// expand_selection
// ---------------------------------------------------------------------------------------------

#[test]
fn expand_plain_clip_returns_only_itself() {
    let mut p = project();
    let a = vid(&p, "a");
    let b = vid(&p, "b");
    let c = vid(&p, "c");
    p.clips.extend([a, b, c]);
    assert_eq!(expand_selection(&p, &ids(&["b"])), ids(&["b"]));
}

#[test]
fn expand_group_member_pulls_whole_group() {
    let mut p = project();
    let a = in_group(vid(&p, "a"), "G");
    let b = in_group(aud(&p, "b"), "G");
    let c = vid(&p, "c");
    p.clips.extend([a, b, c]);
    assert_eq!(expand_selection(&p, &ids(&["b"])), ids(&["a", "b"]));
}

#[test]
fn expand_link_partner_pulls_partner() {
    let mut p = project();
    let x = in_link(vid(&p, "x"), "K");
    let y = in_link(aud(&p, "y"), "K");
    let z = vid(&p, "z");
    p.clips.extend([x, y, z]);
    assert_eq!(expand_selection(&p, &ids(&["y"])), ids(&["x", "y"]));
}

#[test]
fn expand_group_then_link_chains_to_linked_partner() {
    // g1 and g2 share group G; g2 is linked to l2 (not in the group). Selecting g1 must reach l2.
    let mut p = project();
    let g1 = in_group(vid(&p, "g1"), "G");
    let g2 = in_link(in_group(vid(&p, "g2"), "G"), "K");
    let l2 = in_link(aud(&p, "l2"), "K");
    p.clips.extend([g1, g2, l2]);
    assert_eq!(
        expand_selection(&p, &ids(&["g1"])),
        ids(&["g1", "g2", "l2"])
    );
}

#[test]
fn expand_link_then_group_chains_to_group_members() {
    // l1 is linked to g1, and g1 is in group G with g2. Selecting l1 must reach g2.
    let mut p = project();
    let l1 = in_link(aud(&p, "l1"), "K");
    let g1 = in_group(in_link(vid(&p, "g1"), "K"), "G");
    let g2 = in_group(vid(&p, "g2"), "G");
    p.clips.extend([l1, g1, g2]);
    assert_eq!(
        expand_selection(&p, &ids(&["l1"])),
        ids(&["l1", "g1", "g2"])
    );
}

#[test]
fn expand_ignores_unknown_ids() {
    let mut p = project();
    let a = vid(&p, "a");
    p.clips.push(a);
    assert_eq!(expand_selection(&p, &ids(&["nope", "a"])), ids(&["a"]));
}

#[test]
fn expand_all_unknown_ids_gives_empty() {
    let p = project();
    assert!(expand_selection(&p, &ids(&["x", "y"])).is_empty());
}

#[test]
fn expand_empty_selection_gives_empty() {
    let mut p = project();
    let a = vid(&p, "a");
    p.clips.push(a);
    assert!(expand_selection(&p, &[]).is_empty());
}

#[test]
fn expand_has_no_duplicates() {
    let mut p = project();
    let a = in_group(vid(&p, "a"), "G");
    let b = in_group(aud(&p, "b"), "G");
    p.clips.extend([a, b]);
    assert_eq!(
        expand_selection(&p, &ids(&["a", "a", "b"])),
        ids(&["a", "b"])
    );
}

#[test]
fn expand_orders_by_project_clip_order_not_input_order() {
    let mut p = project();
    let x = in_group(vid(&p, "x"), "G");
    let y = vid(&p, "y");
    let z = in_group(vid(&p, "z"), "G");
    p.clips.extend([x, y, z]);
    assert_eq!(expand_selection(&p, &ids(&["z", "x"])), ids(&["x", "z"]));
}

// ---------------------------------------------------------------------------------------------
// group_clips
// ---------------------------------------------------------------------------------------------

#[test]
fn group_two_clips_creates_group_named_group_1() {
    let mut p = project();
    let a = vid(&p, "a");
    let b = aud(&p, "b");
    p.clips.extend([a, b]);
    let gid = group_clips(&mut p, &ids(&["a", "b"])).expect("group ok");
    assert_eq!(group_of(&p, "a"), Some(gid.clone()));
    assert_eq!(group_of(&p, "b"), Some(gid.clone()));
    assert_eq!(p.groups.len(), 1);
    let g = p.groups.first().expect("one group");
    assert_eq!(g.id, gid);
    assert_eq!(g.name, "Group 1");
    assert_eq!(g.color, None);
}

#[test]
fn group_single_clip_is_invalid_and_atomic() {
    let mut p = project();
    let a = vid(&p, "a");
    p.clips.push(a);
    let before = p.clone();
    let r = group_clips(&mut p, &ids(&["a"]));
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
    assert_eq!(p, before);
}

#[test]
fn group_with_only_unknown_ids_is_invalid() {
    let mut p = project();
    let a = vid(&p, "a");
    p.clips.push(a);
    let r = group_clips(&mut p, &ids(&["nope", "a"]));
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn group_one_clip_with_link_partner_counts_partner_and_succeeds() {
    let mut p = project();
    let a = in_link(vid(&p, "a"), "K");
    let b = in_link(aud(&p, "b"), "K");
    p.clips.extend([a, b]);
    let gid = group_clips(&mut p, &ids(&["a"])).expect("expansion adds the partner");
    assert_eq!(group_of(&p, "a"), Some(gid.clone()));
    assert_eq!(group_of(&p, "b"), Some(gid));
}

#[test]
fn group_second_group_is_named_group_2() {
    let mut p = project();
    for id in ["a", "b", "c", "d"] {
        let c = vid(&p, id);
        p.clips.push(c);
    }
    group_clips(&mut p, &ids(&["a", "b"])).expect("first group");
    group_clips(&mut p, &ids(&["c", "d"])).expect("second group");
    let names: Vec<String> = p.groups.iter().map(|g| g.name.clone()).collect();
    assert!(names.contains(&"Group 2".to_string()), "names {names:?}");
    assert_eq!(p.groups.len(), 2);
}

#[test]
fn group_regrouping_one_member_absorbs_old_group_via_expansion() {
    // G1 = {a, b}. Grouping [b, c] expands b to its whole group, so a joins the new group too
    // and G1 is removed (it is left with no members).
    let mut p = project();
    for id in ["a", "b", "c"] {
        let c = vid(&p, id);
        p.clips.push(c);
    }
    let g1 = group_clips(&mut p, &ids(&["a", "b"])).expect("first group");
    let g2 = group_clips(&mut p, &ids(&["b", "c"])).expect("second group");
    assert_ne!(g1, g2);
    assert_eq!(group_of(&p, "a"), Some(g2.clone()));
    assert_eq!(group_of(&p, "b"), Some(g2.clone()));
    assert_eq!(group_of(&p, "c"), Some(g2.clone()));
    assert!(p.groups.iter().all(|g| g.id != g1));
    assert_eq!(p.groups.len(), 1);
}

#[test]
fn group_regrouping_keeps_no_clip_in_two_groups() {
    // Membership is exclusive: after regrouping, each clip has at most one group id and only
    // existing groups are referenced.
    let mut p = project();
    for id in ["a", "b", "c", "d"] {
        let c = vid(&p, id);
        p.clips.push(c);
    }
    group_clips(&mut p, &ids(&["a", "b", "c"])).expect("first group");
    group_clips(&mut p, &ids(&["c", "d"])).expect("second group");
    for c in &p.clips {
        if let Some(g) = &c.group_id {
            assert!(p.groups.iter().any(|x| &x.id == g), "dangling group {g}");
        }
    }
    assert_eq!(p.groups.len(), 1);
}

#[test]
fn group_expansion_pulls_existing_group_mates_into_new_group() {
    // a is in G with b. Grouping [a, c] expands to include b, so b moves with them.
    let mut p = project();
    let a = in_group(vid(&p, "a"), "G");
    let b = in_group(vid(&p, "b"), "G");
    let c = vid(&p, "c");
    p.clips.extend([a, b, c]);
    p.groups.push(clipcheck_timeline::model::Group {
        id: "G".to_string(),
        name: "Old".to_string(),
        color: None,
    });
    let gid = group_clips(&mut p, &ids(&["a", "c"])).expect("group ok");
    assert_eq!(group_of(&p, "a"), Some(gid.clone()));
    assert_eq!(group_of(&p, "b"), Some(gid.clone()));
    assert_eq!(group_of(&p, "c"), Some(gid));
    assert!(p.groups.iter().all(|g| g.id != "G"));
}

// ---------------------------------------------------------------------------------------------
// ungroup
// ---------------------------------------------------------------------------------------------

#[test]
fn ungroup_frees_all_members_in_clip_order_and_removes_group() {
    let mut p = project();
    let a = in_group(vid(&p, "a"), "G");
    let b = in_group(vid(&p, "b"), "G");
    let c = in_group(aud(&p, "c"), "G");
    p.clips.extend([a, b, c]);
    p.groups.push(clipcheck_timeline::model::Group {
        id: "G".to_string(),
        name: "Group 1".to_string(),
        color: None,
    });
    let freed = ungroup(&mut p, &ids(&["a"])).expect("ungroup ok");
    assert_eq!(freed, ids(&["a", "b", "c"]));
    assert!(p.groups.is_empty());
    assert!(p.clips.iter().all(|c| c.group_id.is_none()));
}

#[test]
fn ungroup_by_any_member_frees_the_whole_group() {
    let mut p = project();
    let a = in_group(vid(&p, "a"), "G");
    let b = in_group(vid(&p, "b"), "G");
    p.clips.extend([a, b]);
    p.groups.push(clipcheck_timeline::model::Group {
        id: "G".to_string(),
        name: "Group 1".to_string(),
        color: None,
    });
    let freed = ungroup(&mut p, &ids(&["b"])).expect("ungroup ok");
    assert_eq!(freed, ids(&["a", "b"]));
}

#[test]
fn ungroup_clip_without_group_is_empty_and_no_change() {
    let mut p = project();
    let a = vid(&p, "a");
    p.clips.push(a);
    let before = p.clone();
    let freed = ungroup(&mut p, &ids(&["a"])).expect("ok");
    assert!(freed.is_empty());
    assert_eq!(p, before);
}

#[test]
fn ungroup_unknown_id_is_empty() {
    let mut p = project();
    let freed = ungroup(&mut p, &ids(&["nope"])).expect("ok");
    assert!(freed.is_empty());
}

#[test]
fn ungroup_leaves_link_partners_alone() {
    let mut p = project();
    let a = in_link(in_group(vid(&p, "a"), "G"), "K");
    let b = in_group(aud(&p, "b"), "G");
    let x = in_link(aud(&p, "x"), "K");
    p.clips.extend([a, b, x]);
    p.groups.push(clipcheck_timeline::model::Group {
        id: "G".to_string(),
        name: "Group 1".to_string(),
        color: None,
    });
    let freed = ungroup(&mut p, &ids(&["a"])).expect("ok");
    assert_eq!(freed, ids(&["a", "b"]));
    assert_eq!(link_of(&p, "a"), Some("K".to_string()));
    assert_eq!(link_of(&p, "x"), Some("K".to_string()));
}

// ---------------------------------------------------------------------------------------------
// link_clips / unlink_clips
// ---------------------------------------------------------------------------------------------

#[test]
fn link_two_clips_shares_one_fresh_link_id() {
    let mut p = project();
    let a = vid(&p, "a");
    let b = aud(&p, "b");
    p.clips.extend([a, b]);
    link_clips(&mut p, &ids(&["a", "b"])).expect("link ok");
    let la = link_of(&p, "a").expect("a linked");
    assert_eq!(link_of(&p, "b"), Some(la));
}

#[test]
fn link_single_clip_is_invalid_and_atomic() {
    let mut p = project();
    let a = vid(&p, "a");
    p.clips.push(a);
    let before = p.clone();
    let r = link_clips(&mut p, &ids(&["a"]));
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
    assert_eq!(p, before);
}

#[test]
fn link_ignores_unknown_ids_but_needs_two_known_clips() {
    let mut p = project();
    let a = vid(&p, "a");
    let b = aud(&p, "b");
    p.clips.extend([a, b]);
    link_clips(&mut p, &ids(&["a", "b", "nope"])).expect("unknown ids ignored");
    assert!(link_of(&p, "a").is_some());

    let mut q = project();
    let a2 = vid(&q, "a");
    q.clips.push(a2);
    let r = link_clips(&mut q, &ids(&["a", "nope"]));
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn link_relinking_orphans_previous_partner() {
    // a and b were linked (K1). Linking [a, c] gives a and c a new id, so b is left alone.
    let mut p = project();
    let a = in_link(vid(&p, "a"), "K1");
    let b = in_link(aud(&p, "b"), "K1");
    let c = vid(&p, "c");
    p.clips.extend([a, b, c]);
    link_clips(&mut p, &ids(&["a", "c"])).expect("relink ok");
    assert_eq!(link_of(&p, "b"), None);
    let la = link_of(&p, "a").expect("a linked");
    assert_ne!(la, "K1");
    assert_eq!(link_of(&p, "c"), Some(la));
}

#[test]
fn link_relinking_keeps_partners_still_linked_to_each_other() {
    // a, b and c share K1. Linking [a, d] leaves b and c still linked on K1.
    let mut p = project();
    let a = in_link(vid(&p, "a"), "K1");
    let b = in_link(aud(&p, "b"), "K1");
    let c = in_link(vid(&p, "c"), "K1");
    let d = aud(&p, "d");
    p.clips.extend([a, b, c, d]);
    link_clips(&mut p, &ids(&["a", "d"])).expect("relink ok");
    assert_eq!(link_of(&p, "b"), Some("K1".to_string()));
    assert_eq!(link_of(&p, "c"), Some("K1".to_string()));
    assert_ne!(link_of(&p, "a"), Some("K1".to_string()));
    assert_eq!(link_of(&p, "a"), link_of(&p, "d"));
}

#[test]
fn unlink_clears_clip_and_its_partner() {
    let mut p = project();
    let a = in_link(vid(&p, "a"), "K");
    let b = in_link(aud(&p, "b"), "K");
    p.clips.extend([a, b]);
    unlink_clips(&mut p, &ids(&["a"])).expect("unlink ok");
    assert_eq!(link_of(&p, "a"), None);
    assert_eq!(link_of(&p, "b"), None);
}

#[test]
fn unlink_clears_every_member_of_the_link() {
    let mut p = project();
    let a = in_link(vid(&p, "a"), "K");
    let b = in_link(aud(&p, "b"), "K");
    let c = in_link(aud(&p, "c"), "K");
    p.clips.extend([a, b, c]);
    unlink_clips(&mut p, &ids(&["b"])).expect("unlink ok");
    assert_eq!(link_of(&p, "a"), None);
    assert_eq!(link_of(&p, "b"), None);
    assert_eq!(link_of(&p, "c"), None);
}

#[test]
fn unlink_unlinked_or_unknown_is_noop_ok() {
    let mut p = project();
    let a = vid(&p, "a");
    p.clips.push(a);
    let before = p.clone();
    unlink_clips(&mut p, &ids(&["a", "nope"])).expect("noop ok");
    assert_eq!(p, before);
}

// ---------------------------------------------------------------------------------------------
// detach_audio
// ---------------------------------------------------------------------------------------------

#[test]
fn detach_audio_on_audio_clip_is_invalid_and_unchanged() {
    let mut p = project();
    let v = in_link(vid(&p, "v"), "K");
    let aa = in_link(aud(&p, "aa"), "K");
    p.clips.extend([v, aa]);
    let before = p.clone();
    let r = detach_audio(&mut p, "aa");
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
    assert_eq!(p, before);
}

#[test]
fn detach_audio_unknown_clip_is_not_found() {
    let mut p = project();
    assert_eq!(
        detach_audio(&mut p, "nope"),
        Err(EditError::NotFound("nope".to_string()))
    );
}

#[test]
fn detach_audio_mutes_and_unlinks_all_audio_partners() {
    let mut p = project();
    let v = in_link(vid(&p, "v"), "K");
    let aa = in_link(aud(&p, "aa"), "K");
    let aa2 = in_link(clip(&p, "aa2", "A2", ClipKind::Audio), "K");
    p.clips.extend([v, aa, aa2]);
    let muted = detach_audio(&mut p, "v").expect("detach ok");
    assert_eq!(muted, ids(&["aa", "aa2"]));
    assert!(get(&p, "aa").audio.muted);
    assert!(get(&p, "aa2").audio.muted);
    assert_eq!(link_of(&p, "v"), None);
    assert_eq!(link_of(&p, "aa"), None);
    assert_eq!(link_of(&p, "aa2"), None);
}

#[test]
fn detach_audio_video_without_link_returns_empty_and_no_change() {
    let mut p = project();
    let v = vid(&p, "v");
    p.clips.push(v);
    let before = p.clone();
    let muted = detach_audio(&mut p, "v").expect("ok");
    assert!(muted.is_empty());
    assert_eq!(p, before);
}

#[test]
fn detach_audio_with_only_video_partner_returns_empty_and_keeps_link() {
    let mut p = project();
    let v = in_link(vid(&p, "v"), "K");
    let v2 = in_link(vid(&p, "v2"), "K");
    p.clips.extend([v, v2]);
    let before = p.clone();
    let muted = detach_audio(&mut p, "v").expect("ok");
    assert!(muted.is_empty());
    assert_eq!(p, before);
    assert_eq!(link_of(&p, "v"), Some("K".to_string()));
}

#[test]
fn detach_audio_on_locked_audio_track_is_locked_and_atomic() {
    let mut p = project();
    let v = in_link(vid(&p, "v"), "K");
    let aa = in_link(aud(&p, "aa"), "K");
    p.clips.extend([v, aa]);
    let a1 = tid(&p, "A1");
    p.track_mut(&a1).expect("A1").locked = true;
    let before = p.clone();
    let r = detach_audio(&mut p, "v");
    assert_eq!(r, Err(EditError::Locked(a1)));
    assert_eq!(p, before);
    assert!(!get(&p, "aa").audio.muted);
}

// ---------------------------------------------------------------------------------------------
// edit_group
// ---------------------------------------------------------------------------------------------

/// Project with clips a and b grouped; returns (project, group id).
fn grouped_pair() -> (Project, String) {
    let mut p = project();
    let a = vid(&p, "a");
    let b = aud(&p, "b");
    p.clips.extend([a, b]);
    let gid = group_clips(&mut p, &ids(&["a", "b"])).expect("group ok");
    (p, gid)
}

fn group_name(p: &Project, gid: &str) -> String {
    p.groups
        .iter()
        .find(|g| g.id == gid)
        .map(|g| g.name.clone())
        .expect("group exists")
}

fn group_color(p: &Project, gid: &str) -> Option<String> {
    p.groups
        .iter()
        .find(|g| g.id == gid)
        .and_then(|g| g.color.clone())
}

#[test]
fn edit_group_rename_trims_surrounding_whitespace() {
    let (mut p, gid) = grouped_pair();
    edit_group(&mut p, &gid, Some("  Hero  ".to_string()), None).expect("rename ok");
    assert_eq!(group_name(&p, &gid), "Hero");
}

#[test]
fn edit_group_blank_name_is_invalid_and_keeps_name() {
    let (mut p, gid) = grouped_pair();
    let before = group_name(&p, &gid);
    let r = edit_group(&mut p, &gid, Some("   ".to_string()), None);
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
    assert_eq!(group_name(&p, &gid), before);
}

#[test]
fn edit_group_name_of_60_chars_ok_and_61_invalid() {
    let (mut p, gid) = grouped_pair();
    let sixty = "x".repeat(60);
    edit_group(&mut p, &gid, Some(sixty.clone()), None).expect("60 ok");
    assert_eq!(group_name(&p, &gid), sixty);
    let r = edit_group(&mut p, &gid, Some("x".repeat(61)), None);
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
    assert_eq!(group_name(&p, &gid), sixty);
}

#[test]
fn edit_group_length_counts_characters_not_bytes() {
    // 60 accented characters are 120 bytes but still 60 characters: allowed.
    let (mut p, gid) = grouped_pair();
    let name = "é".repeat(60);
    edit_group(&mut p, &gid, Some(name.clone()), None).expect("60 chars ok");
    assert_eq!(group_name(&p, &gid), name);
}

#[test]
fn edit_group_sets_colour() {
    let (mut p, gid) = grouped_pair();
    edit_group(&mut p, &gid, None, Some(Some("#ff0000".to_string()))).expect("colour ok");
    assert_eq!(group_color(&p, &gid), Some("#ff0000".to_string()));
}

#[test]
fn edit_group_some_none_clears_colour() {
    let (mut p, gid) = grouped_pair();
    edit_group(&mut p, &gid, None, Some(Some("#ff0000".to_string()))).expect("set ok");
    edit_group(&mut p, &gid, None, Some(None)).expect("clear ok");
    assert_eq!(group_color(&p, &gid), None);
}

#[test]
fn edit_group_none_colour_leaves_colour_unchanged() {
    let (mut p, gid) = grouped_pair();
    edit_group(&mut p, &gid, None, Some(Some("#00ff00".to_string()))).expect("set ok");
    edit_group(&mut p, &gid, Some("Renamed".to_string()), None).expect("rename ok");
    assert_eq!(group_color(&p, &gid), Some("#00ff00".to_string()));
    assert_eq!(group_name(&p, &gid), "Renamed");
}

#[test]
fn edit_group_unknown_group_is_not_found() {
    let (mut p, _gid) = grouped_pair();
    let r = edit_group(&mut p, "nope", Some("X".to_string()), None);
    assert_eq!(r, Err(EditError::NotFound("nope".to_string())));
}

#[test]
fn edit_group_invalid_name_does_not_apply_colour() {
    let (mut p, gid) = grouped_pair();
    let before = p.clone();
    let r = edit_group(
        &mut p,
        &gid,
        Some(String::new()),
        Some(Some("#123456".to_string())),
    );
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
    assert_eq!(p, before);
}

// ---------------------------------------------------------------------------------------------
// adjust_group_gain
// ---------------------------------------------------------------------------------------------

/// Group G with audio clip "aa" (gain 0) and video clip "v"; audio clip "other" outside G.
fn gain_project() -> (Project, String) {
    let mut p = project();
    let aa = aud(&p, "aa");
    let v = vid(&p, "v");
    let other = aud(&p, "other");
    p.clips.extend([aa, v, other]);
    let gid = group_clips(&mut p, &ids(&["aa", "v"])).expect("group ok");
    (p, gid)
}

fn gain(p: &Project, id: &str) -> f64 {
    get(p, id).audio.gain_db
}

#[test]
fn adjust_gain_adds_delta_to_audio_members_only() {
    let (mut p, gid) = gain_project();
    adjust_group_gain(&mut p, &gid, -6.0).expect("adjust ok");
    assert_eq!(gain(&p, "aa"), -6.0);
    assert_eq!(gain(&p, "v"), 0.0);
    assert_eq!(gain(&p, "other"), 0.0);
}

#[test]
fn adjust_gain_clamps_at_plus_12() {
    let (mut p, gid) = gain_project();
    p.clip_mut("aa").expect("aa").audio.gain_db = 10.0;
    adjust_group_gain(&mut p, &gid, 5.0).expect("adjust ok");
    assert_eq!(gain(&p, "aa"), 12.0);
}

#[test]
fn adjust_gain_clamps_at_minus_60() {
    let (mut p, gid) = gain_project();
    p.clip_mut("aa").expect("aa").audio.gain_db = -55.0;
    adjust_group_gain(&mut p, &gid, -10.0).expect("adjust ok");
    assert_eq!(gain(&p, "aa"), -60.0);
}

#[test]
fn adjust_gain_exact_small_change() {
    let (mut p, gid) = gain_project();
    adjust_group_gain(&mut p, &gid, 1.5).expect("adjust ok");
    assert_eq!(gain(&p, "aa"), 1.5);
}

#[test]
fn adjust_gain_nan_delta_is_invalid_and_unchanged() {
    let (mut p, gid) = gain_project();
    let before = p.clone();
    let r = adjust_group_gain(&mut p, &gid, f64::NAN);
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
    assert_eq!(p, before);
}

#[test]
fn adjust_gain_infinite_delta_is_invalid() {
    let (mut p, gid) = gain_project();
    let r = adjust_group_gain(&mut p, &gid, f64::INFINITY);
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn adjust_gain_unknown_group_is_not_found() {
    let (mut p, _gid) = gain_project();
    let r = adjust_group_gain(&mut p, "nope", 1.0);
    assert_eq!(r, Err(EditError::NotFound("nope".to_string())));
}

#[test]
fn adjust_gain_nan_clip_gain_becomes_zero_db() {
    // Doc on clamp_gain: a NaN gain (from a hostile file) becomes 0 dB.
    let (mut p, gid) = gain_project();
    p.clip_mut("aa").expect("aa").audio.gain_db = f64::NAN;
    adjust_group_gain(&mut p, &gid, -3.0).expect("adjust ok");
    assert_eq!(gain(&p, "aa"), 0.0);
}
