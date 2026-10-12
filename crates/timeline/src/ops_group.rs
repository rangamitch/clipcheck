//! Groups and A/V links. Owner: worker `tl-group`.
//!
//! Same contract as `ops_basic`: validate first, then mutate, so a refused edit leaves the project
//! unchanged.

use std::collections::{HashMap, HashSet};

use crate::error::{EditError, EditResult};
use crate::model::{new_id, ClipKind, Group, Project};

/// Expands a selection with the group members and link partners of every listed clip (iterating
/// until stable, so a linked partner's group is included too). Unknown ids are ignored. The result
/// is deduplicated and ordered by clip order in `p.clips`.
pub fn expand_selection(p: &Project, ids: &[String]) -> Vec<String> {
    let want: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let mut sel: HashSet<&str> = p
        .clips
        .iter()
        .filter(|c| want.contains(c.id.as_str()))
        .map(|c| c.id.as_str())
        .collect();

    // Each pass adds at least one clip or stops, so the loop runs at most `clips.len()` times.
    loop {
        let groups: HashSet<&str> = p
            .clips
            .iter()
            .filter(|c| sel.contains(c.id.as_str()))
            .filter_map(|c| c.group_id.as_deref())
            .collect();
        let links: HashSet<&str> = p
            .clips
            .iter()
            .filter(|c| sel.contains(c.id.as_str()))
            .filter_map(|c| c.link_id.as_deref())
            .collect();
        let added: Vec<&str> = p
            .clips
            .iter()
            .filter(|c| {
                !sel.contains(c.id.as_str())
                    && (c.group_id.as_deref().is_some_and(|g| groups.contains(g))
                        || c.link_id.as_deref().is_some_and(|l| links.contains(l)))
            })
            .map(|c| c.id.as_str())
            .collect();
        if added.is_empty() {
            break;
        }
        sel.extend(added);
    }

    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for c in &p.clips {
        if sel.contains(c.id.as_str()) && seen.insert(c.id.as_str()) {
            out.push(c.id.clone());
        }
    }
    out
}

/// Groups clips (`Ctrl+G`): needs at least 2 clips after expansion; any clip already in a group
/// is first removed from it (no nesting; a group left with fewer than 2 members is deleted).
/// Creates a [`crate::model::Group`] named "Group N" and returns its id.
pub fn group_clips(p: &mut Project, ids: &[String]) -> EditResult<String> {
    let members = expand_selection(p, ids);
    if members.len() < 2 {
        return Err(EditError::Invalid(
            "grouping needs at least 2 clips".to_string(),
        ));
    }
    let member_set: HashSet<&str> = members.iter().map(String::as_str).collect();

    // Take the members out of any group they were in.
    for c in p.clips.iter_mut() {
        if member_set.contains(c.id.as_str()) {
            c.group_id = None;
        }
    }

    // Delete groups left with fewer than 2 members and clear the lone remaining member.
    let small: HashSet<String> = {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for c in &p.clips {
            if let Some(g) = c.group_id.as_deref() {
                *counts.entry(g).or_insert(0) += 1;
            }
        }
        p.groups
            .iter()
            .filter(|g| counts.get(g.id.as_str()).copied().unwrap_or(0) < 2)
            .map(|g| g.id.clone())
            .collect()
    };
    if !small.is_empty() {
        for c in p.clips.iter_mut() {
            if c.group_id.as_ref().is_some_and(|g| small.contains(g)) {
                c.group_id = None;
            }
        }
        p.groups.retain(|g| !small.contains(&g.id));
    }

    let gid = new_id();
    let name = format!("Group {}", p.groups.len() + 1);
    p.groups.push(Group {
        id: gid.clone(),
        name,
        color: None,
    });
    for c in p.clips.iter_mut() {
        if member_set.contains(c.id.as_str()) {
            c.group_id = Some(gid.clone());
        }
    }
    Ok(gid)
}

/// Ungroups (`Ctrl+Shift+G`): clears `group_id` on every member of the groups the listed clips
/// belong to and deletes those groups. Link partners are untouched. Returns the freed clip ids.
pub fn ungroup(p: &mut Project, ids: &[String]) -> EditResult<Vec<String>> {
    let gids: HashSet<String> = ids
        .iter()
        .filter_map(|id| p.clip(id))
        .filter_map(|c| c.group_id.clone())
        .collect();
    if gids.is_empty() {
        return Ok(Vec::new());
    }
    let mut freed: Vec<String> = Vec::new();
    for c in p.clips.iter_mut() {
        if c.group_id.as_ref().is_some_and(|g| gids.contains(g)) {
            c.group_id = None;
            freed.push(c.id.clone());
        }
    }
    p.groups.retain(|g| !gids.contains(&g.id));
    Ok(freed)
}

/// Links the listed clips (needs >= 2): gives them one fresh `link_id`. Unknown ids are ignored.
/// A previous link partner left alone with no other member has its `link_id` cleared.
pub fn link_clips(p: &mut Project, ids: &[String]) -> EditResult<()> {
    let want: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let set: HashSet<String> = p
        .clips
        .iter()
        .filter(|c| want.contains(c.id.as_str()))
        .map(|c| c.id.clone())
        .collect();
    if set.len() < 2 {
        return Err(EditError::Invalid(
            "linking needs at least 2 clips".to_string(),
        ));
    }

    let old: HashSet<String> = p
        .clips
        .iter()
        .filter(|c| set.contains(&c.id))
        .filter_map(|c| c.link_id.clone())
        .collect();
    let lid = new_id();
    for c in p.clips.iter_mut() {
        if set.contains(&c.id) {
            c.link_id = Some(lid.clone());
        }
    }

    // Old partners that now stand alone (exactly one clip still carries their link id).
    let orphans: HashSet<String> = {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for c in &p.clips {
            if let Some(l) = c.link_id.as_deref() {
                *counts.entry(l).or_insert(0) += 1;
            }
        }
        old.into_iter()
            .filter(|l| counts.get(l.as_str()) == Some(&1))
            .collect()
    };
    if !orphans.is_empty() {
        for c in p.clips.iter_mut() {
            if c.link_id.as_ref().is_some_and(|l| orphans.contains(l)) {
                c.link_id = None;
            }
        }
    }
    Ok(())
}

/// Unlinks the listed clips and their current link partners: clears `link_id` on all of them.
pub fn unlink_clips(p: &mut Project, ids: &[String]) -> EditResult<()> {
    let listed: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let links: HashSet<String> = p
        .clips
        .iter()
        .filter(|c| listed.contains(c.id.as_str()))
        .filter_map(|c| c.link_id.clone())
        .collect();
    for c in p.clips.iter_mut() {
        let partner = c.link_id.as_ref().is_some_and(|l| links.contains(l));
        if partner || listed.contains(c.id.as_str()) {
            c.link_id = None;
        }
    }
    Ok(())
}

/// Detach audio (F-GRP-6): for a Video clip, unlinks it from its audio partners and mutes those
/// audio clips (`audio.muted = true`). Returns the ids of the audio clips muted. Refuses with
/// `Locked` when a partner's track is locked. With no audio partners it returns an empty list and
/// changes nothing.
pub fn detach_audio(p: &mut Project, video_clip_id: &str) -> EditResult<Vec<String>> {
    let (kind, link) = match p.clip(video_clip_id) {
        Some(v) => (v.kind, v.link_id.clone()),
        None => return Err(EditError::NotFound(video_clip_id.to_string())),
    };
    if kind != ClipKind::Video {
        return Err(EditError::Invalid(format!(
            "clip {video_clip_id} is not a video clip"
        )));
    }
    let Some(link) = link else {
        return Ok(Vec::new());
    };

    // Validate first: collect audio partners and check their tracks are not locked.
    let partners: Vec<(String, String)> = p
        .clips
        .iter()
        .filter(|c| c.kind == ClipKind::Audio && c.link_id.as_deref() == Some(link.as_str()))
        .map(|c| (c.id.clone(), c.track_id.clone()))
        .collect();
    if partners.is_empty() {
        return Ok(Vec::new());
    }
    for (_, track_id) in &partners {
        if p.track(track_id).is_some_and(|t| t.locked) {
            return Err(EditError::Locked(track_id.clone()));
        }
    }

    let partner_ids: HashSet<&str> = partners.iter().map(|(id, _)| id.as_str()).collect();
    for c in p.clips.iter_mut() {
        if c.id == video_clip_id {
            c.link_id = None;
        } else if partner_ids.contains(c.id.as_str()) {
            c.audio.muted = true;
            c.link_id = None;
        }
    }
    Ok(partners.into_iter().map(|(id, _)| id).collect())
}

/// Renames a group and/or sets its colour. `color: Some(None)` clears the colour. The name is
/// trimmed and must be 1 to 60 characters.
pub fn edit_group(
    p: &mut Project,
    group_id: &str,
    name: Option<String>,
    color: Option<Option<String>>,
) -> EditResult<()> {
    if !p.groups.iter().any(|g| g.id == group_id) {
        return Err(EditError::NotFound(group_id.to_string()));
    }
    let new_name = match name {
        Some(n) => {
            let t = n.trim();
            let len = t.chars().count();
            if len == 0 || len > 60 {
                return Err(EditError::Invalid(
                    "group name must be 1 to 60 characters".to_string(),
                ));
            }
            Some(t.to_string())
        }
        None => None,
    };
    if let Some(g) = p.groups.iter_mut().find(|g| g.id == group_id) {
        if let Some(n) = new_name {
            g.name = n;
        }
        if let Some(c) = color {
            g.color = c;
        }
    }
    Ok(())
}

/// Adds `delta_db` to the clip gain of every audio member of the group (clamped to -60..=12).
/// A non-finite `delta_db` is refused.
pub fn adjust_group_gain(p: &mut Project, group_id: &str, delta_db: f64) -> EditResult<()> {
    if !delta_db.is_finite() {
        return Err(EditError::Invalid(
            "gain change must be a finite number of dB".to_string(),
        ));
    }
    if !p.groups.iter().any(|g| g.id == group_id) {
        return Err(EditError::NotFound(group_id.to_string()));
    }
    for c in p.clips.iter_mut() {
        if c.kind == ClipKind::Audio && c.group_id.as_deref() == Some(group_id) {
            c.audio.gain_db = clamp_gain(c.audio.gain_db + delta_db);
        }
    }
    Ok(())
}

/// Clamps a gain to -60..=12 dB. A NaN (from a hostile file) becomes 0 dB.
fn clamp_gain(v: f64) -> f64 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(-60.0, 12.0)
    }
}
