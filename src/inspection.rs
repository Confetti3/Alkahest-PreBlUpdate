//! Lossless structural inspection for packages whose semantic schema is not
//! known yet. Typed visitors can attach children to the same document later;
//! the raw payload is never discarded in the meantime.

use std::{fmt::Write as _, fs, path::Path, str::FromStr};

use anyhow::Context;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tiger_pkg::{TagHash, package::UEntryHeader, package_manager};

pub const SCHEMA_VERSION: &str = "alkahest-inspection/v1";
const INLINE_RAW_LIMIT: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectionKind {
    Tag,
    Activity,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectionStatus {
    Known,
    UnknownSemantic,
    Raw,
}

#[derive(Debug, Clone, Serialize)]
pub struct InspectNode {
    pub name: String,
    pub type_name: String,
    pub source_offset: usize,
    pub encoded_size: usize,
    pub status: InspectionStatus,
    pub value: Option<serde_json::Value>,
    pub children: Vec<InspectNode>,
    pub target: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TagIdentity {
    pub hash: String,
    pub package_id: u16,
    pub entry_index: u16,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntryMetadata {
    pub reference: String,
    pub file_type: u8,
    pub file_subtype: u8,
    pub declared_size: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct RawPayload {
    pub byte_length: usize,
    pub sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline_hex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidecar: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TagInspection {
    pub schema_version: &'static str,
    pub era: &'static str,
    pub kind: InspectionKind,
    pub tag: TagIdentity,
    pub entry: EntryMetadata,
    pub fields: Vec<InspectNode>,
    pub raw_payload: RawPayload,
    pub diagnostics: Vec<String>,
}

/// A serializable inspection plus the exact payload needed for the interactive
/// hex view and sidecar export.
pub struct InspectionDocument {
    pub record: TagInspection,
    bytes: Vec<u8>,
}

impl InspectionDocument {
    pub fn read(tag: TagHash, kind: InspectionKind) -> anyhow::Result<Self> {
        let manager = package_manager();
        let entry = manager
            .get_entry(tag)
            .with_context(|| format!("Tag {tag} does not have a package entry"))?;
        let name = manager.get_tag_name(tag);
        let bytes = manager
            .read_tag(tag)
            .with_context(|| format!("Failed to read tag {tag}"))?;

        Ok(Self::from_parts(tag, name, entry, bytes, kind))
    }

    fn from_parts(
        tag: TagHash,
        name: Option<String>,
        entry: UEntryHeader,
        bytes: Vec<u8>,
        kind: InspectionKind,
    ) -> Self {
        let mut diagnostics = Vec::new();
        if entry.file_size as usize != bytes.len() {
            diagnostics.push(format!(
                "Package entry declares {} bytes but returned {} bytes",
                entry.file_size,
                bytes.len()
            ));
        }

        let hash = hex_sha256(&bytes);
        let payload_len = bytes.len();
        Self {
            record: TagInspection {
                schema_version: SCHEMA_VERSION,
                era: alkahest_core::SHADOWKEEP_ERA.id,
                kind,
                tag: TagIdentity {
                    hash: tag.to_string(),
                    package_id: tag.pkg_id(),
                    entry_index: tag.entry_index(),
                    name,
                },
                entry: EntryMetadata {
                    reference: format!("{:08X}", entry.reference),
                    file_type: entry.file_type,
                    file_subtype: entry.file_subtype,
                    declared_size: entry.file_size,
                },
                fields: structural_fields(entry.reference, &bytes),
                raw_payload: RawPayload {
                    byte_length: payload_len,
                    sha256: hash,
                    inline_hex: None,
                    sidecar: None,
                },
                diagnostics,
            },
            bytes,
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn write_json(&self, output: &Path) -> anyhow::Result<()> {
        let parent = output.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .with_context(|| format!("Creating export directory {}", parent.display()))?;

        let mut record = self.record.clone();
        if self.bytes.len() <= INLINE_RAW_LIMIT {
            record.raw_payload.inline_hex = Some(encode_hex(&self.bytes));
        } else {
            let sidecar_name = format!(
                "{}.bin",
                output
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("payload")
            );
            let sidecar_path = output.with_file_name(&sidecar_name);
            fs::write(&sidecar_path, &self.bytes)
                .with_context(|| format!("Writing raw sidecar {}", sidecar_path.display()))?;
            record.raw_payload.sidecar = Some(sidecar_name);
        }

        let json = serde_json::to_vec_pretty(&record).context("Serializing inspection JSON")?;
        fs::write(output, json)
            .with_context(|| format!("Writing inspection JSON {}", output.display()))?;
        Ok(())
    }
}

fn structural_fields(class_id: u32, bytes: &[u8]) -> Vec<InspectNode> {
    let mut children = match class_id {
        // This layout is independently present in the preserved pre-BL tree.
        // It is deliberately not inferred from later Destiny 2 layouts.
        0x8080_8E8E => shadowkeep_activity_fields(bytes),
        _ => Vec::new(),
    };

    if let Some(file_size) = read_u64(bytes, 0).filter(|file_size| *file_size == bytes.len() as u64)
    {
        children.push(node(
            "file_size",
            "u64",
            0,
            8,
            InspectionStatus::Known,
            Some(serde_json::json!(file_size)),
            None,
        ));
    }

    children = fill_unowned_ranges(children, bytes.len());
    vec![InspectNode {
        name: "payload".to_owned(),
        type_name: "bytes".to_owned(),
        source_offset: 0,
        encoded_size: bytes.len(),
        status: InspectionStatus::Raw,
        value: None,
        children,
        target: None,
    }]
}

/// Shadowkeep / Season of Arrivals layout for `SActivity` (class 0x80808E8E).
/// Field names and offsets come from the read-only pre-BL implementation; the
/// `unk*` members deliberately retain their non-semantic names.
fn shadowkeep_activity_fields(bytes: &[u8]) -> Vec<InspectNode> {
    let mut fields = Vec::new();
    for (name, offset, status) in [
        ("location_name", 0x08, InspectionStatus::Known),
        ("unkc", 0x0C, InspectionStatus::UnknownSemantic),
        ("unk10", 0x10, InspectionStatus::UnknownSemantic),
        ("unk14", 0x14, InspectionStatus::UnknownSemantic),
        ("unk70", 0x70, InspectionStatus::UnknownSemantic),
    ] {
        if let Some(value) = read_u32(bytes, offset) {
            fields.push(node(
                name,
                "FnvHash",
                offset,
                4,
                status,
                Some(serde_json::json!(format!("{value:08X}"))),
                None,
            ));
        }
    }

    if let Some(pointer) = resource_pointer_value(bytes, 0x18) {
        fields.push(node(
            "unk18",
            "ResourcePointer",
            0x18,
            8,
            InspectionStatus::UnknownSemantic,
            Some(pointer.0),
            pointer.1,
        ));
    }
    if let Some((value, target)) = wide_hash_value(bytes, 0x20) {
        fields.push(node(
            "destination",
            "WideHash",
            0x20,
            16,
            InspectionStatus::Known,
            Some(value),
            target,
        ));
    }

    for (name, offset, element_type) in [("unk40", 0x40, "80808926"), ("unk50", 0x50, "80808924")] {
        if let Some((value, target)) = vector_value(bytes, offset) {
            fields.push(node(
                name,
                format!("Vec<{element_type}>"),
                offset,
                16,
                InspectionStatus::UnknownSemantic,
                Some(value),
                target,
            ));
        }
    }

    if let Some(values) = read_u32_array::<4>(bytes, 0x60) {
        fields.push(node(
            "unk60",
            "[u32; 4]",
            0x60,
            16,
            InspectionStatus::UnknownSemantic,
            Some(serde_json::json!(values)),
            None,
        ));
    }
    if let Some(value) = read_u32(bytes, 0x74) {
        fields.push(node(
            "unk74",
            "TagHash",
            0x74,
            4,
            InspectionStatus::UnknownSemantic,
            Some(serde_json::json!(format!("{value:08X}"))),
            Some(format!("{value:08X}")),
        ));
    }
    if bytes.len() > 0x78 {
        fields.push(raw_range(0x78, (bytes.len() - 0x78).min(8)));
    }
    fields
}

fn node(
    name: impl Into<String>,
    type_name: impl Into<String>,
    source_offset: usize,
    encoded_size: usize,
    status: InspectionStatus,
    value: Option<serde_json::Value>,
    target: Option<String>,
) -> InspectNode {
    InspectNode {
        name: name.into(),
        type_name: type_name.into(),
        source_offset,
        encoded_size,
        status,
        value,
        children: Vec::new(),
        target,
    }
}

fn fill_unowned_ranges(mut nodes: Vec<InspectNode>, payload_len: usize) -> Vec<InspectNode> {
    nodes.sort_by_key(|node| node.source_offset);
    let mut complete = Vec::with_capacity(nodes.len() + 2);
    let mut cursor = 0usize;
    for node in nodes {
        if node.source_offset > cursor {
            complete.push(raw_range(cursor, node.source_offset - cursor));
        }
        cursor = cursor.max(node.source_offset.saturating_add(node.encoded_size));
        complete.push(node);
    }
    if cursor < payload_len {
        complete.push(raw_range(cursor, payload_len - cursor));
    }
    complete
}

fn raw_range(offset: usize, size: usize) -> InspectNode {
    node(
        format!("raw_{offset:08X}_{:08X}", offset.saturating_add(size)),
        "bytes",
        offset,
        size,
        InspectionStatus::Raw,
        None,
        None,
    )
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    bytes
        .get(offset..offset.saturating_add(4))?
        .try_into()
        .ok()
        .map(u32::from_le_bytes)
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    bytes
        .get(offset..offset.saturating_add(8))?
        .try_into()
        .ok()
        .map(u64::from_le_bytes)
}

fn read_u32_array<const N: usize>(bytes: &[u8], offset: usize) -> Option<[u32; N]> {
    let mut values = [0; N];
    for (index, value) in values.iter_mut().enumerate() {
        *value = read_u32(bytes, offset.checked_add(index.checked_mul(4)?)?)?;
    }
    Some(values)
}

fn resource_pointer_value(
    bytes: &[u8],
    offset: usize,
) -> Option<(serde_json::Value, Option<String>)> {
    let relative_offset = read_u64(bytes, offset)? as i64;
    if relative_offset == 0 || relative_offset == i64::MAX {
        return Some((
            serde_json::json!({ "valid": false, "relative_offset": relative_offset }),
            None,
        ));
    }
    let target = relative_target(offset, relative_offset)?;
    let resource_type = target
        .checked_sub(4)
        .and_then(|type_offset| read_u32(bytes, type_offset));
    Some((
        serde_json::json!({
            "valid": resource_type.is_some(),
            "relative_offset": relative_offset,
            "resource_type": resource_type.map(|value| format!("{value:08X}")),
        }),
        resource_type.map(|_| format!("payload+0x{target:X}")),
    ))
}

fn vector_value(bytes: &[u8], offset: usize) -> Option<(serde_json::Value, Option<String>)> {
    let count = read_u64(bytes, offset)? as i64;
    let relative_offset = read_u64(bytes, offset.checked_add(8)?)? as i64;
    let pointer_base = offset.checked_add(16)?;
    let target = relative_target(pointer_base, relative_offset);
    Some((
        serde_json::json!({
            "count": count,
            "relative_offset": relative_offset,
        }),
        target.map(|target| format!("payload+0x{target:X}")),
    ))
}

fn wide_hash_value(bytes: &[u8], offset: usize) -> Option<(serde_json::Value, Option<String>)> {
    let hash32 = read_u32(bytes, offset)?;
    let is_hash32 = read_u32(bytes, offset.checked_add(4)?)?;
    let hash64 = read_u64(bytes, offset.checked_add(8)?)?;
    if is_hash32 != 0 {
        let tag = format!("{hash32:08X}");
        Some((
            serde_json::json!({ "kind": "hash32", "value": tag }),
            Some(tag),
        ))
    } else {
        Some((
            serde_json::json!({ "kind": "hash64", "value": format!("{hash64:016X}") }),
            None,
        ))
    }
}

fn relative_target(base: usize, relative_offset: i64) -> Option<usize> {
    let relative_offset = isize::try_from(relative_offset).ok()?;
    base.checked_add_signed(relative_offset)
}

pub fn export_one(tag: &str, output: &Path, kind: InspectionKind) -> anyhow::Result<()> {
    let tag = TagHash::from_str(tag).with_context(|| format!("Invalid tag hash '{tag}'"))?;
    InspectionDocument::read(tag, kind)?.write_json(output)
}

pub fn export_all(output_dir: &Path) -> anyhow::Result<()> {
    if output_dir.exists() {
        let mut entries = fs::read_dir(output_dir)
            .with_context(|| format!("Reading export directory {}", output_dir.display()))?;
        if entries.next().is_some() {
            anyhow::bail!(
                "Refusing to overwrite non-empty export directory {}",
                output_dir.display()
            );
        }
    } else {
        fs::create_dir_all(output_dir)
            .with_context(|| format!("Creating export directory {}", output_dir.display()))?;
    }

    let manager = package_manager();
    let mut tags = (u8::MIN..=u8::MAX)
        .flat_map(|file_type| manager.get_all_by_type(file_type, None))
        .map(|(tag, _)| tag)
        .collect::<Vec<_>>();
    tags.sort_unstable();
    tags.dedup();

    let mut failures = Vec::new();
    let mut exported = 0usize;
    for tag in tags.iter().copied() {
        let output = output_dir.join(format!("{tag}.json"));
        match InspectionDocument::read(tag, InspectionKind::Tag)
            .and_then(|document| document.write_json(&output))
        {
            Ok(()) => exported += 1,
            Err(error) => failures.push(serde_json::json!({
                "tag": tag.to_string(),
                "error": format!("{error:#}"),
            })),
        }
    }

    let manifest = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "era": alkahest_core::SHADOWKEEP_ERA.id,
        "total_tags": tags.len(),
        "exported_tags": exported,
        "failures": failures,
    });
    fs::write(
        output_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).context("Serializing export manifest")?,
    )
    .context("Writing export manifest")?;
    Ok(())
}

pub fn format_hex_page(bytes: &[u8], offset: usize, length: usize) -> String {
    let start = offset.min(bytes.len()) & !0xF;
    let end = start.saturating_add(length).min(bytes.len());
    let mut output = String::new();

    for (line_offset, line) in bytes[start..end].chunks(16).enumerate() {
        let absolute_offset = start + line_offset * 16;
        let _ = write!(output, "{absolute_offset:08X}  ");
        for index in 0..16 {
            if let Some(byte) = line.get(index) {
                let _ = write!(output, "{byte:02X} ");
            } else {
                output.push_str("   ");
            }
            if index == 7 {
                output.push(' ');
            }
        }
        output.push_str(" | ");
        for byte in line {
            output.push(if byte.is_ascii_graphic() || *byte == b' ' {
                *byte as char
            } else {
                '.'
            });
        }
        output.push('\n');
    }

    output
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    encode_hex(&digest)
}

#[cfg(test)]
mod tests {
    use super::{format_hex_page, structural_fields};

    #[test]
    fn hex_page_has_offsets_hex_and_ascii() {
        let output = format_hex_page(b"AB\0CD", 0, 16);
        assert!(output.contains("00000000"));
        assert!(output.contains("41 42 00 43 44"));
        assert!(output.contains("AB.CD"));
    }

    #[test]
    fn activity_layout_preserves_all_bytes_without_overlaps() {
        let mut bytes = vec![0u8; 0x90];
        let payload_len = bytes.len() as u64;
        bytes[..8].copy_from_slice(&payload_len.to_le_bytes());
        bytes[0x08..0x0C].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        bytes[0x20..0x24].copy_from_slice(&0x80A1_25BBu32.to_le_bytes());
        bytes[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
        bytes[0x40..0x48].copy_from_slice(&2i64.to_le_bytes());
        bytes[0x48..0x50].copy_from_slice(&0x20i64.to_le_bytes());

        let root = structural_fields(0x8080_8E8E, &bytes).pop().unwrap();
        let mut expected_offset = 0usize;
        for child in root.children {
            assert_eq!(child.source_offset, expected_offset, "{}", child.name);
            expected_offset += child.encoded_size;
        }
        assert_eq!(expected_offset, bytes.len());
    }
}
