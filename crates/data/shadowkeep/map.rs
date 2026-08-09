use glam::{Mat4, Quat, Vec4};
use tiger_parse::{ResourcePointer, tiger_type};
use tiger_pkg::TagHash;

use crate::{
    shadowkeep::geometry::SShadowkeepStaticMeshInstances,
    tag::{Tag, WideTag},
    tfx::features::cubemap::SCubemapComponent,
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

/// Table-local cubemap volume payload class `0x80806B7F`.
///
/// Arrivals stores the authored volume constants after a 0x20-byte resource
/// prefix. The two matrices and texture fields are consumed unchanged by the
/// preserved cubemap techniques; the normalized component keeps the existing
/// renderer independent of this era-specific disk layout.
#[derive(Clone, Debug)]
#[tiger_type(id = 0x80806B7F, size = 0x1A4)]
pub struct SShadowkeepCubemapPlacement {
    #[tiger(offset = 0x20)]
    pub volume_extents: Vec4,
    pub volume_center: Vec4,
    pub unk40: f32,
    pub unk44: [u32; 3],
    pub fade_near: Vec4,
    pub fade_far: Vec4,
    pub shape: Vec4,
    pub fade_power: f32,
    #[tiger(offset = 0xC0)]
    pub volume_to_world: Mat4,
    pub cubemap_to_world: Mat4,
    pub probes_resolution: [u32; 3],
    pub unk14c: f32,
    pub intensity: Vec4,
    pub relighting: Vec4,
    pub unk170: Vec4,
    #[tiger(offset = 0x198)]
    pub texture_cube_specular_ibl: TagHash,
    pub texture_cube_alpha: TagHash,
    pub texture_voxel_diffuse: TagHash,
}

impl SShadowkeepCubemapPlacement {
    pub fn normalized(&self) -> SCubemapComponent {
        SCubemapComponent {
            unk0: 0,
            _pad8: 0,
            volume_extents: self.volume_extents,
            volume_center: self.volume_center,
            unk30: self.unk40,
            unk34: self.unk44,
            unk40_fade1: self.fade_near,
            unk50_fade2: self.fade_far,
            unk60: self.shape,
            unk70: self.fade_power,
            unkb0: self.volume_to_world,
            unkf0: Vec4::ZERO,
            unk100: Vec4::ZERO,
            unk110: self.cubemap_to_world,
            probes_resolution: self.probes_resolution,
            unk15c: self.unk14c,
            unk160: self.intensity,
            unk170: self.relighting,
            unk180: self.unk170,
            unk190: 0,
            unk198: 0,
            unk19a: 0,
            texture_cube_specular_ibl: self.texture_cube_specular_ibl,
            texture_cube_alpha: self.texture_cube_alpha,
            texture_voxel_diffuse: self.texture_voxel_diffuse,
            unk1a8: TagHash::NONE,
            unk1ac: [0; 3],
        }
    }
}
