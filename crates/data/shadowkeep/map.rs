use glam::{Quat, Vec4};
use tiger_parse::{ResourcePointer, tiger_type};
use tiger_pkg::TagHash;

use crate::{
    shadowkeep::geometry::SShadowkeepStaticMeshInstances,
    tag::{Tag, WideTag},
};

/// Legacy map root (`0x80807DAE`), not the post-BL bubble parent.
#[derive(Debug)]
#[tiger_type(id = 0x80807DAE, size = 0x50)]
pub struct SShadowkeepBubbleParent {
    pub file_size: u64,
    pub child_map: TagHash,
    pub unkc: u32,
    pub unk10: u64,
    pub map_name: u32,
    #[tiger(offset = 0x40)]
    pub trailing_records: Vec<SShadowkeepBubbleParentRecord>,
}

#[derive(Debug)]
#[tiger_type(id = 0x80809644)]
pub struct SShadowkeepBubbleParentRecord {
    pub unk0: [u32; 4],
}

#[derive(Debug)]
#[tiger_type(id = 0x808091E0, size = 0x18)]
pub struct SShadowkeepBubbleDefinition {
    pub file_size: u64,
    pub map_resources: Vec<WideTag<SShadowkeepMapContainer>>,
}

#[derive(Debug)]
#[tiger_type(id = 0x80808A54, size = 0x38)]
pub struct SShadowkeepMapContainer {
    pub file_size: u64,
    #[tiger(offset = 0x28)]
    pub data_tables: Vec<TagHash>,
}

#[derive(Debug)]
#[tiger_type(id = 0x808099D6)]
pub struct SShadowkeepMapDataTable {
    pub file_size: u64,
    pub data_entries: Vec<SShadowkeepMapDataTableEntry>,
}

#[derive(Clone, Debug)]
#[tiger_type(id = 0x808099D8)]
pub struct SShadowkeepMapDataTableEntry {
    pub entity: TagHash,
    pub unk4: [u32; 3],
    pub rotation: Quat,
    pub translation: Vec4,
    pub unk30: [u32; 11],
    pub unk5c: f32,
    pub unk60: u32,
    pub unk64: u32,
    pub unk68: [u32; 2],
    pub world_id: u64,
    pub data_resource: ResourcePointer,
    pub unk80: [u32; 4],
}

/// Resource payload class `0x808071B3`: a static placement points to this
/// header after its table-local prefix.
#[derive(Debug)]
#[tiger_type(id = 0x80806EF4, size = 0x24)]
pub struct SShadowkeepStaticPlacement {
    pub unk0: u64,
    pub instances: Tag<SShadowkeepStaticMeshInstances>,
}

/// Table-local terrain payload class `0x8080714B`.
#[derive(Clone, Debug)]
#[tiger_type(size = 0x20)]
pub struct SShadowkeepTerrainPlacement {
    #[tiger(offset = 0x10)]
    pub unk10: u16,
    pub unk12: u16,
    pub identifier: u32,
    pub terrain: TagHash,
    pub terrain_bounds: TagHash,
}
