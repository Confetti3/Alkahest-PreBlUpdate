use std::ops::Range;

use glam::{Quat, Vec2, Vec3, Vec4};
use tiger_parse::tiger_type;
use tiger_pkg::TagHash;

use crate::{
    tag::Tag,
    tfx::{LodCategory, PrimitiveType, RenderStage},
};

pub const SHADOWKEEP_RENDER_STAGE_COUNT: usize = 23;

/// Converts an Arrivals stage index into the matching shared stage.  Index 23
/// is deliberately rejected: ComputeSkinning was introduced later.
pub fn render_stage_from_legacy(value: u8) -> Option<RenderStage> {
    (value < SHADOWKEEP_RENDER_STAGE_COUNT as u8)
        .then(|| RenderStage::try_from(value).ok())
        .flatten()
}

pub fn primitive_type_from_legacy(value: u8) -> Option<PrimitiveType> {
    PrimitiveType::try_from(value).ok()
}

pub fn lod_category_from_legacy(value: u8) -> Option<LodCategory> {
    match value {
        0 => Some(LodCategory::Lod_0_0),
        1 => Some(LodCategory::Lod_0_1),
        2 => Some(LodCategory::Lod_0_2),
        3 => Some(LodCategory::Lod_0_3),
        4 => Some(LodCategory::Lod_1_0),
        7 => Some(LodCategory::Lod_2_0),
        8 => Some(LodCategory::Lod_2_1),
        9 => Some(LodCategory::Lod_3_0),
        10 => Some(LodCategory::Lod_Detail),
        _ => None,
    }
}

#[derive(Debug)]
#[tiger_type(id = 0x808071A7)]
pub struct SShadowkeepStaticMesh {
    pub file_size: u64,
    pub opaque_meshes: Tag<SShadowkeepStaticMeshData>,
    pub unkc: u32,
    pub techniques: Vec<TagHash>,
    pub special_meshes: Vec<SShadowkeepStaticSpecialMesh>,
    pub unk30: [u32; 2],
    pub unk38: [f32; 6],
    pub unk50: Vec3,
    pub unk5c: f32,
    pub mesh_offset: Vec3,
    pub mesh_scale: f32,
    pub texture_coordinate_scale: Vec2,
    pub texture_coordinate_offset: Vec2,
}

#[derive(Debug)]
#[tiger_type(id = 0x80807194, size = 0x60)]
pub struct SShadowkeepStaticMeshData {
    pub file_size: u64,
    pub mesh_groups: Vec<SShadowkeepStaticMeshGroup>,
    pub parts: Vec<SShadowkeepStaticMeshPart>,
    pub buffers: Vec<(TagHash, TagHash, TagHash, TagHash)>,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x8080719A)]
pub struct SShadowkeepStaticMeshPart {
    pub index_start: u32,
    pub index_count: u32,
    pub buffer_index: u8,
    pub unk9: u8,
    pub lod_category: u8,
    pub primitive_type: u8,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x8080719B)]
pub struct SShadowkeepStaticMeshGroup {
    pub part_index: u16,
    pub render_stage: u8,
    pub unk3: u8,
    pub input_layout_index: u8,
    pub unk5: u8,
    pub unk6: u16,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x8080966D, size = 0x98)]
pub struct SShadowkeepStaticMeshInstances {
    #[tiger(offset = 0x40)]
    pub transforms: Vec<SShadowkeepStaticInstanceTransform>,
    pub unk50: [u32; 2],
    pub statics: Vec<TagHash>,
    pub instance_groups: Vec<SShadowkeepStaticMeshInstanceGroup>,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x80807190)]
pub struct SShadowkeepStaticMeshInstanceGroup {
    pub instance_count: u16,
    pub instance_start: u16,
    pub static_index: u16,
    pub unk6: u16,
}

impl SShadowkeepStaticMeshInstanceGroup {
    pub fn transform_range(&self) -> Range<usize> {
        self.instance_start as usize..(self.instance_start as usize + self.instance_count as usize)
    }
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x808071A3)]
pub struct SShadowkeepStaticInstanceTransform {
    pub rotation: Quat,
    pub translation: Vec3,
    pub scale: Vec3,
    pub unk28: u32,
    pub unk2c: u32,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x80807193, size = 0x20)]
pub struct SShadowkeepStaticSpecialMesh {
    pub render_stage: u8,
    pub input_layout_index: u8,
    pub unk2: u16,
    pub lod_category: u8,
    pub unk5: i8,
    pub primitive_type: u8,
    pub unk7: u8,
    pub index_buffer: TagHash,
    pub vertex0_buffer: TagHash,
    pub vertex1_buffer: TagHash,
    pub index_start: u32,
    pub index_count: u32,
    pub technique: TagHash,
}

#[derive(Debug)]
// Legacy source labelled this record 0x88, but the final owned Vec at 0x80
// occupies 0x10 bytes.  Preserve all owned bytes rather than truncate it.
#[tiger_type(id = 0x8080714F, size = 0x90)]
pub struct SShadowkeepTerrain {
    pub file_size: u64,
    pub unk8: u64,
    pub unk10: Vec4,
    pub unk20: Vec4,
    pub position_offset: Vec4,
    #[tiger(offset = 0x58)]
    pub mesh_groups: Vec<SShadowkeepTerrainMeshGroup>,
    pub vertex0_buffer: TagHash,
    pub vertex1_buffer: TagHash,
    pub index_buffer: TagHash,
    pub unk_technique1: TagHash,
    pub unk_technique2: TagHash,
    #[tiger(offset = 0x80)]
    pub mesh_parts: Vec<SShadowkeepTerrainMeshPart>,
}

#[derive(Debug)]
#[tiger_type(id = 0x80807154)]
pub struct SShadowkeepTerrainMeshGroup {
    pub unk0: Vec4,
    pub unk10: f32,
    pub unk14: f32,
    pub unk18: f32,
    pub unk1c: u32,
    pub texcoord_transform: Vec4,
    pub unk30: [u32; 8],
    pub dyemap: TagHash,
    pub unk54: [u32; 3],
}

#[derive(Debug)]
#[tiger_type(id = 0x80807152)]
pub struct SShadowkeepTerrainMeshPart {
    pub technique: TagHash,
    pub index_start: u32,
    pub index_count: u16,
    pub group_index: u8,
    pub detail_level: u8,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x808073A5, size = 0xA0)]
pub struct SShadowkeepDynamicModel {
    pub file_size: u64,
    pub unk8: u64,
    pub meshes: Vec<SShadowkeepDynamicMesh>,
    pub unk20: Vec4,
    #[tiger(offset = 0x50)]
    pub model_scale: Vec4,
    pub model_offset: Vec4,
    pub texcoord_scale: Vec2,
    pub texcoord_offset: Vec2,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x80807378, size = 0x88)]
pub struct SShadowkeepDynamicMesh {
    pub vertex0_buffer: TagHash,
    pub vertex1_buffer: TagHash,
    pub buffer2: TagHash,
    pub buffer3: TagHash,
    pub index_buffer: TagHash,
    pub unk14: u32,
    pub parts: Vec<SShadowkeepDynamicMeshPart>,
    pub part_range_per_render_stage: [u16; SHADOWKEEP_RENDER_STAGE_COUNT + 1],
    pub input_layout_per_render_stage: [u16; SHADOWKEEP_RENDER_STAGE_COUNT],
    pub pad86: [u8; 2],
}

impl SShadowkeepDynamicMesh {
    pub fn range_for_stage(&self, stage: u8) -> Option<Range<usize>> {
        let stage = stage as usize;
        (stage < SHADOWKEEP_RENDER_STAGE_COUNT).then(|| {
            self.part_range_per_render_stage[stage] as usize
                ..self.part_range_per_render_stage[stage + 1] as usize
        })
    }
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x8080737E)]
pub struct SShadowkeepDynamicMeshPart {
    pub technique: TagHash,
    pub variant_shader_index: u16,
    pub primitive_type: u8,
    pub unk7: u8,
    pub index_start: u32,
    pub index_count: u32,
    pub unk10: u32,
    pub external_identifier: u16,
    pub unk16: u16,
    pub unk18: u8,
    pub unk19: u8,
    pub unk1a: u8,
    pub lod_category: u8,
    pub unk1c: u32,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x808072C4)]
pub struct SShadowkeepDynamicMaterialVariant {
    pub technique_count: u32,
    pub technique_start: u32,
    pub unk8: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_stages_stop_before_compute_skinning() {
        assert_eq!(
            render_stage_from_legacy(0),
            Some(RenderStage::GenerateGbuffer)
        );
        assert_eq!(render_stage_from_legacy(22), Some(RenderStage::WorldForces));
        assert_eq!(render_stage_from_legacy(23), None);
    }

    #[test]
    fn legacy_lod_values_do_not_invent_missing_variants() {
        assert_eq!(lod_category_from_legacy(0), Some(LodCategory::Lod_0_0));
        assert_eq!(lod_category_from_legacy(5), None);
        assert_eq!(lod_category_from_legacy(10), Some(LodCategory::Lod_Detail));
    }

    #[test]
    fn static_instance_ranges_use_the_legacy_count_and_start() {
        let group = SShadowkeepStaticMeshInstanceGroup {
            instance_count: 3,
            instance_start: 7,
            static_index: 0,
            unk6: 0,
        };
        assert_eq!(group.transform_range(), 7..10);
    }
}
