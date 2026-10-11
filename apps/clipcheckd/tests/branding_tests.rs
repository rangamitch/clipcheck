//! Verifies the Windows resources that build.rs embeds into clipcheckd.exe:
//! the application icon group (id 1) and its images, the VERSIONINFO block
//! and the application manifest. The exe is parsed by hand with std only.
//!
//! The crate is a binary crate, so these tests read the built exe bytes
//! rather than calling into the crate.
#![cfg(windows)]

const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;
const RT_VERSION: u32 = 16;
const RT_MANIFEST: u32 = 24;
const HIGH_BIT: u32 = 0x8000_0000;

struct Section {
    virtual_size: u32,
    virtual_address: u32,
    raw_size: u32,
    raw_ptr: u32,
}

struct Pe {
    bytes: Vec<u8>,
    resource_rva: u32,
    sections: Vec<Section>,
}

/// One leaf of the resource tree. `None` means the entry was addressed by name
/// rather than by numeric id, which this crate never uses.
struct Resource {
    type_id: Option<u32>,
    name_id: Option<u32>,
    data: Vec<u8>,
}

struct GroupEntry {
    width: u32,
    height: u32,
    bytes_in_res: u32,
    id: u16,
}

fn u16_le(b: &[u8], off: usize) -> Result<u16, String> {
    match b.get(off..off + 2) {
        Some(s) => Ok(u16::from_le_bytes([s[0], s[1]])),
        None => Err(format!("u16 read out of bounds at offset {off:#x}")),
    }
}

fn u32_le(b: &[u8], off: usize) -> Result<u32, String> {
    match b.get(off..off + 4) {
        Some(s) => Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]])),
        None => Err(format!("u32 read out of bounds at offset {off:#x}")),
    }
}

fn parse_pe(bytes: Vec<u8>) -> Result<Pe, String> {
    if bytes.get(0..2) != Some(b"MZ".as_slice()) {
        return Err("missing MZ DOS signature".into());
    }
    let e_lfanew = u32_le(&bytes, 0x3C)? as usize;
    if bytes.get(e_lfanew..e_lfanew + 4) != Some(b"PE\0\0".as_slice()) {
        return Err(format!(
            "missing PE\\0\\0 signature at e_lfanew={e_lfanew:#x}"
        ));
    }
    let coff = e_lfanew + 4;
    let number_of_sections = u16_le(&bytes, coff + 2)? as usize;
    let size_of_optional_header = u16_le(&bytes, coff + 16)? as usize;
    let optional = coff + 20;
    // DataDirectory starts at 96 bytes into the PE32 optional header and at
    // 112 bytes into the PE32+ one.
    let data_dir_base = match u16_le(&bytes, optional)? {
        0x10B => 96,
        0x20B => 112,
        other => return Err(format!("unknown optional header magic {other:#x}")),
    };
    // Index 2 is the resource table; each entry is {RVA u32, Size u32}.
    let resource_entry = optional + data_dir_base + 2 * 8;
    let resource_rva = u32_le(&bytes, resource_entry)?;

    let section_table = optional + size_of_optional_header;
    let mut sections = Vec::with_capacity(number_of_sections);
    for i in 0..number_of_sections {
        let s = section_table + i * 40;
        sections.push(Section {
            virtual_size: u32_le(&bytes, s + 8)?,
            virtual_address: u32_le(&bytes, s + 12)?,
            raw_size: u32_le(&bytes, s + 16)?,
            raw_ptr: u32_le(&bytes, s + 20)?,
        });
    }
    Ok(Pe {
        bytes,
        resource_rva,
        sections,
    })
}

fn rva_to_offset(pe: &Pe, rva: u32) -> Option<usize> {
    pe.sections
        .iter()
        .find(|s| {
            let span = s.virtual_size.max(s.raw_size);
            rva >= s.virtual_address && rva - s.virtual_address < span
        })
        .map(|s| (rva - s.virtual_address + s.raw_ptr) as usize)
}

/// Walks the three-level resource tree (type -> name/id -> language) and
/// returns every leaf data entry.
fn collect_resources(pe: &Pe) -> Result<Vec<Resource>, String> {
    let root =
        rva_to_offset(pe, pe.resource_rva).ok_or("resource RVA is not inside any section")?;
    let mut out = Vec::new();
    walk_dir(pe, root, root, &mut Vec::new(), &mut out)?;
    Ok(out)
}

fn walk_dir(
    pe: &Pe,
    root: usize,
    dir: usize,
    path: &mut Vec<Option<u32>>,
    out: &mut Vec<Resource>,
) -> Result<(), String> {
    if path.len() > 3 {
        return Err("resource tree is deeper than three levels".into());
    }
    // IMAGE_RESOURCE_DIRECTORY: named entries count at +12, id entries at +14.
    let named = u16_le(&pe.bytes, dir + 12)? as usize;
    let ids = u16_le(&pe.bytes, dir + 14)? as usize;
    for i in 0..(named + ids) {
        // IMAGE_RESOURCE_DIRECTORY_ENTRY: Name/Id u32, OffsetToData u32.
        let entry = dir + 16 + i * 8;
        let name = u32_le(&pe.bytes, entry)?;
        let offset = u32_le(&pe.bytes, entry + 4)?;
        let key = if name & HIGH_BIT != 0 {
            None
        } else {
            Some(name)
        };
        path.push(key);
        if offset & HIGH_BIT != 0 {
            walk_dir(pe, root, root + (offset & !HIGH_BIT) as usize, path, out)?;
        } else {
            // IMAGE_RESOURCE_DATA_ENTRY: OffsetToData (RVA) u32, Size u32, CodePage, Reserved.
            let data_entry = root + offset as usize;
            let rva = u32_le(&pe.bytes, data_entry)?;
            let size = u32_le(&pe.bytes, data_entry + 4)? as usize;
            let start =
                rva_to_offset(pe, rva).ok_or("resource data RVA is not inside any section")?;
            let data = pe
                .bytes
                .get(start..start + size)
                .ok_or("resource data extends past end of file")?
                .to_vec();
            out.push(Resource {
                type_id: path[0],
                name_id: path.get(1).copied().flatten(),
                data,
            });
        }
        path.pop();
    }
    Ok(())
}

fn load_exe() -> Pe {
    let bytes = std::fs::read(env!("CARGO_BIN_EXE_clipcheckd")).expect("read built clipcheckd.exe");
    parse_pe(bytes).expect("clipcheckd.exe is a valid PE image")
}

fn resources_of_type(res: &[Resource], type_id: u32) -> Vec<&Resource> {
    res.iter().filter(|r| r.type_id == Some(type_id)).collect()
}

/// Parses an RT_GROUP_ICON payload (ICONDIR followed by 14-byte entries).
fn parse_group_icon(data: &[u8]) -> Result<Vec<GroupEntry>, String> {
    let reserved = u16_le(data, 0)?;
    let kind = u16_le(data, 2)?;
    let count = u16_le(data, 4)? as usize;
    if reserved != 0 || kind != 1 {
        return Err(format!(
            "ICONDIR header invalid: reserved={reserved}, type={kind}"
        ));
    }
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let e = 6 + i * 14;
        // A stored dimension of 0 means 256.
        let dim = |b: u8| if b == 0 { 256 } else { b as u32 };
        entries.push(GroupEntry {
            width: dim(*data.get(e).ok_or("group entry truncated")?),
            height: dim(*data.get(e + 1).ok_or("group entry truncated")?),
            bytes_in_res: u32_le(data, e + 8)?,
            id: u16_le(data, e + 12)?,
        });
    }
    Ok(entries)
}

/// Returns (width, height) of an icon image: PNG-compressed or BMP (DIB).
fn icon_dimensions(data: &[u8]) -> Result<(u32, u32), String> {
    if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        let w = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
        let h = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
        return Ok((w, h));
    }
    // BITMAPINFOHEADER: biWidth at +4, biHeight at +8 (doubled to include the AND mask).
    let header_size = u32_le(data, 0)?;
    if header_size < 40 {
        return Err(format!(
            "icon image is neither PNG nor BMP (header size {header_size})"
        ));
    }
    let w = u32_le(data, 4)? as i32;
    let h = u32_le(data, 8)? as i32 / 2;
    Ok((w.unsigned_abs(), h.unsigned_abs()))
}

fn utf16le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
}

fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Finds the VERSIONINFO string `key` (UTF-16LE, NUL-terminated) and returns
/// the value that follows it after DWORD alignment padding. The value must
/// begin within 64 bytes of the key.
fn version_string_value(data: &[u8], key: &str) -> Option<String> {
    let mut needle = utf16le(key);
    needle.extend_from_slice(&[0, 0]);
    let key_pos = find_bytes(data, &needle)?;
    let key_end = key_pos + needle.len();
    let mut i = key_end;
    while i + 1 < data.len() && data[i] == 0 && data[i + 1] == 0 {
        i += 2;
    }
    if i - key_end > 64 {
        return None;
    }
    let mut units = Vec::new();
    while i + 1 < data.len() {
        let u = u16::from_le_bytes([data[i], data[i + 1]]);
        if u == 0 {
            break;
        }
        units.push(u);
        i += 2;
    }
    String::from_utf16(&units).ok()
}

fn version_info_bytes(pe: &Pe) -> Vec<u8> {
    let res = collect_resources(pe).expect("walk resource tree");
    let versions: Vec<&Resource> = res
        .iter()
        .filter(|r| r.type_id == Some(RT_VERSION) && r.name_id == Some(1))
        .collect();
    assert_eq!(
        versions.len(),
        1,
        "expected exactly one VERSIONINFO (type 16, id 1)"
    );
    versions[0].data.clone()
}

#[test]
fn exe_is_a_valid_pe_image() {
    let pe = load_exe();
    assert!(!pe.sections.is_empty(), "PE image has no sections");
    assert_ne!(
        pe.resource_rva, 0,
        "PE image has no resource data directory"
    );
}

#[test]
fn exe_has_group_icon_with_id_1() {
    let pe = load_exe();
    let res = collect_resources(&pe).expect("walk resource tree");
    let groups: Vec<&Resource> = resources_of_type(&res, RT_GROUP_ICON)
        .into_iter()
        .filter(|r| r.name_id == Some(1))
        .collect();
    assert!(
        !groups.is_empty(),
        "no RT_GROUP_ICON resource with id 1 in clipcheckd.exe"
    );
}

#[test]
fn group_icon_lists_16_32_and_256_entries_resolving_to_icons() {
    let pe = load_exe();
    let res = collect_resources(&pe).expect("walk resource tree");
    let group = res
        .iter()
        .find(|r| r.type_id == Some(RT_GROUP_ICON) && r.name_id == Some(1))
        .expect("RT_GROUP_ICON id 1 present");
    let entries = parse_group_icon(&group.data).expect("parse ICONDIR");

    let widths: Vec<u32> = entries.iter().map(|e| e.width).collect();
    for wanted in [16, 32, 256] {
        assert!(
            widths.contains(&wanted),
            "group icon has no {wanted}px entry; widths = {widths:?}"
        );
    }

    let icons = resources_of_type(&res, RT_ICON);
    for e in &entries {
        let icon = icons
            .iter()
            .find(|r| r.name_id == Some(e.id as u32))
            .unwrap_or_else(|| {
                panic!(
                    "group entry {}x{} id {} has no RT_ICON resource",
                    e.width, e.height, e.id
                )
            });
        assert!(!icon.data.is_empty(), "RT_ICON id {} is empty", e.id);
        assert_eq!(
            icon.data.len() as u32,
            e.bytes_in_res,
            "RT_ICON id {} size does not match group entry bytesinres",
            e.id
        );
    }
}

#[test]
fn rt_icon_resources_include_16_32_and_256_images_matching_group() {
    let pe = load_exe();
    let res = collect_resources(&pe).expect("walk resource tree");
    let icons = resources_of_type(&res, RT_ICON);
    assert!(!icons.is_empty(), "no RT_ICON resources in clipcheckd.exe");

    let mut sizes = Vec::new();
    for icon in &icons {
        let (w, _h) = icon_dimensions(&icon.data).expect("decode RT_ICON image header");
        sizes.push(w);
    }
    for wanted in [16, 32, 256] {
        assert!(
            sizes.contains(&wanted),
            "no RT_ICON image is {wanted}px wide; widths = {sizes:?}"
        );
    }

    let group = res
        .iter()
        .find(|r| r.type_id == Some(RT_GROUP_ICON) && r.name_id == Some(1))
        .expect("RT_GROUP_ICON id 1 present");
    for e in parse_group_icon(&group.data).expect("parse ICONDIR") {
        let icon = icons
            .iter()
            .find(|r| r.name_id == Some(e.id as u32))
            .expect("group entry resolves to RT_ICON");
        let (w, h) = icon_dimensions(&icon.data).expect("decode RT_ICON image header");
        assert_eq!(
            (w, h),
            (e.width, e.height),
            "RT_ICON id {} dimensions differ from its group entry",
            e.id
        );
    }
}

#[test]
fn version_info_product_name_is_clipcheck() {
    let pe = load_exe();
    let data = version_info_bytes(&pe);
    let value = version_string_value(&data, "ProductName");
    assert_eq!(
        value.as_deref(),
        Some("ClipCheck"),
        "VERSIONINFO ProductName"
    );
}

#[test]
fn version_info_file_description_is_clipcheck_recorder() {
    let pe = load_exe();
    let data = version_info_bytes(&pe);
    let value = version_string_value(&data, "FileDescription");
    assert_eq!(
        value.as_deref(),
        Some("ClipCheck Recorder"),
        "VERSIONINFO FileDescription"
    );
}

#[test]
fn version_info_file_version_matches_package_version() {
    let pe = load_exe();
    let data = version_info_bytes(&pe);
    let expected = format!("{}.0", env!("CARGO_PKG_VERSION"));
    let value = version_string_value(&data, "FileVersion");
    assert_eq!(
        value.as_deref(),
        Some(expected.as_str()),
        "VERSIONINFO FileVersion"
    );
}

#[test]
fn manifest_declares_per_monitor_v2_and_as_invoker() {
    let pe = load_exe();
    let res = collect_resources(&pe).expect("walk resource tree");
    let manifests: Vec<&Resource> = resources_of_type(&res, RT_MANIFEST)
        .into_iter()
        .filter(|r| r.name_id == Some(1))
        .collect();
    assert_eq!(
        manifests.len(),
        1,
        "expected one RT_MANIFEST resource with id 1"
    );
    let text = String::from_utf8_lossy(&manifests[0].data).to_lowercase();
    assert!(
        text.contains("permonitorv2"),
        "manifest does not contain PerMonitorV2"
    );
    assert!(
        text.contains("asinvoker"),
        "manifest does not contain asInvoker"
    );
}
