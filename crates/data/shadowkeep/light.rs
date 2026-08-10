//! Shadowkeep / Arrivals lighting resources.
//!
//! These layouts intentionally stay separate from the later-era `tfx::features`
//! lighting types.  The class ids and field ownership changed between the two
//! clients even though both resources ultimately submit a deferred light.

use glam::{Mat4, Quat, Vec4};
use tiger_parse::tiger_type;
use tiger_pkg::TagHash;

use crate::{tag::Tag, tfx::common::AxisAlignedBBox};

#[derive(Clone, Copy, Debug)]
#[tiger_type(id = 0x8080_9F75)]
pub struct SShadowkeepRotationTranslation {
    pub rotation: Quat,
    pub translation: Vec4,
}

#[derive(Clone, Debug)]
#[tiger_type(id = 0x8080_9671, size = 0x18)]
pub struct SShadowkeepOcclusionBounds {
    pub file_size: u64,
    pub bounds: Vec<SShadowkeepObjectOcclusionBounds>,
}

#[derive(Clone, Debug)]
#[tiger_type(id = 0x8080_9673, size = 0x30)]
pub struct SShadowkeepObjectOcclusionBounds {
    pub bb: AxisAlignedBBox,
    pub unk20: [u32; 4],
}

/// Resource class `0x8080713A`, referenced by table entry class `0x80806F5A`.
#[derive(Clone, Debug)]
#[tiger_type(id = 0x8080_713A)]
pub struct SShadowkeepLightCollection {
    pub file_size: u64,
    pub unk8: u64,
    pub bounds: AxisAlignedBBox,
    pub lights: Vec<SShadowkeepLight>,
    pub transforms: Vec<SShadowkeepRotationTranslation>,
    pub light_count: u32,
    pub unk54: u32,
    pub occlusion_bounds: Tag<SShadowkeepOcclusionBounds>,
}

/// Resource class `0x8080713E` (size `0xA0`).
#[derive(Clone, Debug)]
#[tiger_type(id = 0x8080_713E, size = 0xA0)]
pub struct SShadowkeepLight {
    pub unk0: Vec4,
    pub unk10: Vec4,
    pub light_to_world: Mat4,
    pub unk60: [u32; 8],
    pub technique_shading: TagHash,
    pub unk84: TagHash,
    pub unk88: TagHash,
    pub unk8c: TagHash,
    pub unk90: [u32; 4],
}

/// Resource class `0x80807140` used by the preserved shadowing-light path.
#[derive(Clone, Debug)]
#[tiger_type(id = 0x8080_7140, size = 0xA8)]
pub struct SShadowkeepShadowingLight {
    pub unk0: Vec4,
    pub unk10: Vec4,
    pub light_to_world: Mat4,
    pub unk60: [u32; 8],
    pub far_plane: f32,
    pub half_fov: f32,
    pub unkc8: u32,
    pub unkcc: f32,
    pub technique_shading: TagHash,
    pub technique_shading_shadowing: TagHash,
    pub technique_volumetrics: TagHash,
    pub technique_volumetrics_shadowing: TagHash,
    pub unka0: TagHash,
    pub unka4: TagHash,
}
