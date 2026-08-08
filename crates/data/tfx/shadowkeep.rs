//! Shadowkeep/Season of Arrivals render bootstrap layouts.
//!
//! These deliberately live beside, rather than replacing, the later-era
//! `render_globals` layouts.  A matching Rust name is not evidence of a
//! matching on-disk structure.

use std::collections::HashMap;

use anyhow::Context;
use glam::Vec4;
use tiger_parse::{
    Endian, NullString, PackageManagerExt, Pointer, TigerReadable, TigerReader, tiger_type,
};
use tiger_pkg::{TagHash, package_manager};

use crate::{tag::Tag, tfx::ExternIndex};

pub const CLIENT_BOOTSTRAP_NAME: &str = "client_bootstrap_patchable";
pub const CLIENT_BOOTSTRAP_HASH: u32 = 0x80B9_E57D;
pub const CLIENT_BOOTSTRAP_CLASS: u32 = 0x8080_9780;
pub const RENDER_GLOBALS_CLASS: u32 = 0x8080_6CB1;
pub const SCOPE_CLASS: u32 = 0x8080_71F3;
pub const TECHNIQUE_CLASS: u32 = 0x8080_71E8;
pub const DYNAMIC_CONSTANTS_SIZE: usize = 0x68;

/// The complete renderer-facing identity of the preserved Arrivals format.
///
/// It deliberately contains data only.  Renderer code consumes the normalized
/// result rather than treating a similarly named post-BL Rust struct as a
/// compatible on-disk layout.
#[derive(Debug, Clone, Copy, Default)]
pub struct ShadowkeepEraProfile;

impl ShadowkeepEraProfile {
    pub const fn package_version_name(self) -> &'static str {
        "Destiny2Shadowkeep"
    }

    pub const fn renderer_classes(self) -> ShadowkeepRendererClasses {
        ShadowkeepRendererClasses {
            client_bootstrap: CLIENT_BOOTSTRAP_CLASS,
            render_globals: RENDER_GLOBALS_CLASS,
            input_layouts: 0x8080_72A6,
            input_layout_mapping: 0x8080_72A9,
            input_element_sets: 0x8080_72AD,
            input_element_set: 0x8080_72AF,
            input_element: 0x8080_72B2,
            scope: SCOPE_CLASS,
            technique: TECHNIQUE_CLASS,
            lookup_textures: 0x8080_6B99,
        }
    }

    pub const fn geometry_classes(self) -> ShadowkeepGeometryClasses {
        ShadowkeepGeometryClasses {
            bubble_parent: 0x8080_7DAE,
            bubble_definition: 0x8080_91E0,
            map_container: 0x8080_8A54,
            map_table: 0x8080_99D6,
            entity: 0x8080_9C0F,
            entity_resource: 0x8080_9C36,
            rigid_model_component: 0x8080_72B8,
            static_mesh: 0x8080_71A7,
            static_instances: 0x8080_966D,
            terrain: 0x8080_714F,
            dynamic_model: 0x8080_73A5,
        }
    }

    pub fn load_bootstrap(self) -> anyhow::Result<ShadowkeepRenderBootstrap> {
        load_renderer_bootstrap()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ShadowkeepRendererClasses {
    pub client_bootstrap: u32,
    pub render_globals: u32,
    pub input_layouts: u32,
    pub input_layout_mapping: u32,
    pub input_element_sets: u32,
    pub input_element_set: u32,
    pub input_element: u32,
    pub scope: u32,
    pub technique: u32,
    pub lookup_textures: u32,
}

/// Class identities admitted for the core Arrivals geometry path.  They are
/// separate from renderer bootstrap identities because matching later-era
/// Rust names are not format compatibility evidence.
#[derive(Debug, Clone, Copy)]
pub struct ShadowkeepGeometryClasses {
    pub bubble_parent: u32,
    pub bubble_definition: u32,
    pub map_container: u32,
    pub map_table: u32,
    pub entity: u32,
    pub entity_resource: u32,
    pub rigid_model_component: u32,
    pub static_mesh: u32,
    pub static_instances: u32,
    pub terrain: u32,
    pub dynamic_model: u32,
}

/// Normalized, lossless names-to-tags view of the bootstrap.  Names remain
/// package data and are not treated as proof that a post-BL pass is equivalent.
#[derive(Debug, Clone)]
pub struct ShadowkeepRenderBootstrap {
    pub input_layouts: TagHash,
    pub input_layout_count: usize,
    pub scopes: HashMap<String, TagHash>,
    pub pipelines: HashMap<String, TagHash>,
    pub lookup_textures: TagHash,
    pub channel_defaults: TagHash,
}

// The named bootstrap header has a 0x28 declared prefix, while the render
// globals reference is located at 0x4c.  The parser needs the complete owned
// byte range for its offset accounting.
#[tiger_type(id = 0x8080_9780, size = 0x50)]
pub struct SClientBootstrapPatchable {
    #[tiger(offset = 0x4c)]
    pub render_globals: Tag<SShadowkeepRenderGlobals>,
}

#[tiger_type(id = 0x8080_6CB1)]
pub struct SShadowkeepRenderGlobals {
    pub file_size: u64,
    pub input_layouts: Tag<SShadowkeepVertexInputLayouts>,
    _padc: u32,
    pub scopes: Vec<SShadowkeepRenderGlobalScope>,
    pub pipelines: Vec<SShadowkeepRenderGlobalPipeline>,
    pub lookup_textures: Tag<SShadowkeepLookupTextures>,
    /// The legacy renderer treats these as opaque positional channels.  They
    /// are tag hashes, not the later hashed-channel resource family.
    pub channel_defaults: TagHash,
    pub trailing_resource: TagHash,
}

#[tiger_type(id = 0x8080_6B99)]
pub struct SShadowkeepLookupTextures {
    pub file_size: u64,
    pub specular_tint_lookup_texture: TagHash,
    pub specular_lobe_lookup_texture: TagHash,
    pub specular_lobe_3d_lookup_texture: TagHash,
    pub iridescence_lookup_texture: TagHash,
}

#[tiger_type(id = 0x8080_6CB6)]
pub struct SShadowkeepRenderGlobalScope {
    pub name: Pointer<NullString>,
    pub unk8: u32,
    pub scope: TagHash,
}

#[tiger_type(id = 0x8080_6CB5)]
pub struct SShadowkeepRenderGlobalPipeline {
    pub name: Pointer<NullString>,
    pub unk8: u32,
    pub technique: TagHash,
}

#[tiger_type(id = 0x8080_72A6, size = 0x2c)]
pub struct SShadowkeepVertexInputLayouts {
    pub file_size: u64,
    pub unk8: u32,
    pub element_sets: Tag<SShadowkeepVertexInputElementSets>,
    pub elements_10: TagHash,
    pub elements_14: TagHash,
    pub elements_18: TagHash,
    pub elements_1c: TagHash,
    pub elements_20: TagHash,
    pub elements_24: TagHash,
    pub mapping: Tag<SShadowkeepVertexInputLayoutMapping>,
}

#[tiger_type(id = 0x8080_72A9, size = 0x18)]
pub struct SShadowkeepVertexInputLayoutMapping {
    pub file_size: u64,
    pub layouts: Vec<SShadowkeepVertexLayout>,
}

#[tiger_type(id = 0x8080_72AC, size = 0x1c)]
pub struct SShadowkeepVertexLayout {
    pub index: u8,
    #[tiger(offset = 0x8)]
    pub element_0: u32,
    pub element_1: u32,
    pub element_2: u32,
    pub element_3: u32,
    pub unk18: u8,
    pub unk19: u8,
    pub unk1a: u8,
    pub unk1b: u8,
}

#[tiger_type(id = 0x8080_72AD, size = 0x18)]
pub struct SShadowkeepVertexInputElementSets {
    pub file_size: u64,
    pub sets: Vec<SShadowkeepVertexInputElementSet>,
}

#[tiger_type(id = 0x8080_72AF, size = 0x10)]
pub struct SShadowkeepVertexInputElementSet {
    pub elements: Vec<SShadowkeepVertexInputElement>,
}

#[tiger_type(id = 0x8080_72B2, size = 3)]
pub struct SShadowkeepVertexInputElement {
    pub semantic: u8,
    pub semantic_index: u8,
    pub format: u8,
}

#[derive(Clone)]
// The legacy source labels this structure 0x3b8, but six serialized 0x98
// stages beginning at 0x40 occupy 0x3d0 bytes.  Do not impose the legacy
// declaration as an ownership bound until corpus reads establish whether the
// final stage is truncated or the old size label was stale.
#[tiger_type(id = 0x8080_71F3, size = 0x3d0)]
pub struct SShadowkeepScope {
    pub file_size: u64,
    pub name: Pointer<NullString>,
    #[tiger(offset = 0x40)]
    pub stage_pixel: SShadowkeepScopeStage,
    pub stage_vertex: SShadowkeepScopeStage,
    pub stage_geometry: SShadowkeepScopeStage,
    pub stage_hull: SShadowkeepScopeStage,
    pub stage_compute: SShadowkeepScopeStage,
    pub stage_domain: SShadowkeepScopeStage,
}

impl SShadowkeepScope {
    pub fn stages(&self) -> [(&SShadowkeepScopeStage, ShadowkeepShaderStage); 6] {
        [
            (&self.stage_pixel, ShadowkeepShaderStage::Pixel),
            (&self.stage_vertex, ShadowkeepShaderStage::Vertex),
            (&self.stage_geometry, ShadowkeepShaderStage::Geometry),
            (&self.stage_hull, ShadowkeepShaderStage::Hull),
            (&self.stage_compute, ShadowkeepShaderStage::Compute),
            (&self.stage_domain, ShadowkeepShaderStage::Domain),
        ]
    }
}

#[derive(Clone)]
#[tiger_type(size = 0x98)]
pub struct SShadowkeepScopeStage {
    pub unk0: [u32; 4],
    pub unk10: u64,
    pub constants: SShadowkeepDynamicConstants,
    pub unk80: [u32; 6],
}

#[derive(Clone)]
#[tiger_type(id = 0x8080_71E8)]
pub struct SShadowkeepTechnique {
    pub file_size: u64,
    pub bind_mode: ShadowkeepTechniqueBindMode,
    pub unkc: u32,
    pub unk10: u32,
    pub unk14: u32,
    pub used_scopes: ShadowkeepScopeBits,
    pub compatible_scopes: ShadowkeepScopeBits,
    pub states: ShadowkeepPipelineState,
    pub unk24: u32,
    pub unk28: [u32; 8],
    pub shader_vertex: SShadowkeepTechniqueShader,
    pub shader_unk1: SShadowkeepTechniqueShader,
    pub shader_unk2: SShadowkeepTechniqueShader,
    pub shader_geometry: SShadowkeepTechniqueShader,
    pub shader_pixel: SShadowkeepTechniqueShader,
    pub shader_compute: SShadowkeepTechniqueShader,
}

impl SShadowkeepTechnique {
    pub fn shaders(&self) -> [(&SShadowkeepTechniqueShader, ShadowkeepShaderStage); 4] {
        [
            (&self.shader_vertex, ShadowkeepShaderStage::Vertex),
            (&self.shader_geometry, ShadowkeepShaderStage::Geometry),
            (&self.shader_pixel, ShadowkeepShaderStage::Pixel),
            (&self.shader_compute, ShadowkeepShaderStage::Compute),
        ]
    }
}

#[derive(Clone)]
#[tiger_type(size = 0xa0)]
pub struct SShadowkeepTechniqueShader {
    pub shader: TagHash,
    pub unk4: u32,
    pub textures: Vec<SShadowkeepMaterialTextureAssignment>,
    pub unk18: u64,
    pub constants: SShadowkeepDynamicConstants,
    pub unk78: [u32; 6],
}

#[derive(Clone)]
#[tiger_type(id = 0x8080_7211)]
pub struct SShadowkeepMaterialTextureAssignment {
    pub slot: u32,
    pub texture: TagHash,
}

#[derive(Clone)]
#[tiger_type(size = 0x68)]
pub struct SShadowkeepDynamicConstants {
    pub bytecode: Vec<u8>,
    pub bytecode_constants: Vec<Vec4>,
    pub samplers: Vec<SShadowkeepSamplerReference>,
    pub inline_constants: Vec<Vec4>,
    pub unk40: [u32; 8],
    pub constant_buffer_slot: i32,
    pub constant_buffer: TagHash,
}

#[derive(Clone)]
#[tiger_type(id = 0x8080_73F3)]
pub struct SShadowkeepSamplerReference {
    pub sampler: TagHash,
    pub unk4: u32,
    pub unk8: u32,
    pub unkc: u32,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShadowkeepTechniqueBindMode {
    VertexPixel = 1,
    VertexOnly = 2,
    VertexGeometryPixel = 3,
    VertexPixelTessellated = 4,
    VertexOnlyTessellated = 5,
    Compute = 6,
}

impl TigerReadable for ShadowkeepTechniqueBindMode {
    fn read_ds_endian(reader: &mut dyn TigerReader, endian: Endian) -> tiger_parse::Result<Self> {
        match u32::read_ds_endian(reader, endian)? {
            1 => Ok(Self::VertexPixel),
            2 => Ok(Self::VertexOnly),
            3 => Ok(Self::VertexGeometryPixel),
            4 => Ok(Self::VertexPixelTessellated),
            5 => Ok(Self::VertexOnlyTessellated),
            6 => Ok(Self::Compute),
            value => Err(tiger_parse::Error::EnumVariantOutOfRange(value as usize)),
        }
    }
    const SIZE: usize = 4;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ShadowkeepScopeBits(pub u32);

impl TigerReadable for ShadowkeepScopeBits {
    fn read_ds_endian(reader: &mut dyn TigerReader, endian: Endian) -> tiger_parse::Result<Self> {
        Ok(Self(u32::read_ds_endian(reader, endian)?))
    }
    const SIZE: usize = 4;
}

#[derive(Clone, Copy, Debug, Default)]
#[tiger_type(size = 4)]
pub struct ShadowkeepPipelineState(pub u32);

impl ShadowkeepPipelineState {
    pub fn state_indices(self) -> [Option<usize>; 4] {
        let raw = self.0;
        [0, 8, 16, 24].map(|shift| {
            let value = ((raw >> shift) & 0xff) as usize;
            (value & 0x80 != 0).then_some(value & 0x7f)
        })
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShadowkeepShaderStage {
    Pixel,
    Vertex,
    Geometry,
    Hull,
    Compute,
    Domain,
}

/// Translate an encoded Shadowkeep extern index to the renderer's internal
/// extern representation.  Beyond index 23 the later format inserted
/// `CuiDrawingShader`; applying `ExternIndex::try_from` directly would make
/// every subsequent expression address the wrong extern.
pub fn decode_extern_index(encoded: u8) -> Option<ExternIndex> {
    let internal = match encoded {
        0..=23 => encoded,
        24..=96 => encoded.checked_add(1)?,
        _ => return None,
    };
    ExternIndex::try_from(internal).ok()
}

/// Documented legacy defaults.  These values are positional rather than
/// hashed and must remain so until a channel has independent naming evidence.
pub fn global_channel_defaults() -> [Vec4; 256] {
    let mut channels = [Vec4::ONE; 256];
    for index in [10, 82, 83, 97, 98, 100, 127] {
        channels[index] = Vec4::ZERO;
    }
    channels[27] = Vec4::X;
    channels[28] = Vec4::ONE;
    channels[31] = Vec4::ONE;
    channels[32] = Vec4::X;
    channels[33] = Vec4::ONE;
    channels[34] = Vec4::X;
    channels[37] = Vec4::X * 50.0;
    channels[41] = Vec4::X * 50.0;
    channels[84] = Vec4::ONE;
    channels[93] = Vec4::X;
    channels[131] = Vec4::new(0.5, 0.5, 0.3, 0.0);
    channels
}

/// Load the era-correct bootstrap and its mandatory input-layout resources.
/// This is an intentionally narrow capability probe: it proves the corpus
/// contains the foundational Shadowkeep resources without pretending that the
/// current post-BL renderer can consume their unlike scope/technique layouts.
pub fn validate_renderer_bootstrap() -> anyhow::Result<()> {
    load_renderer_bootstrap().map(|_| ())
}

pub fn load_renderer_bootstrap() -> anyhow::Result<ShadowkeepRenderBootstrap> {
    let bootstrap: SClientBootstrapPatchable = package_manager()
        .read_named_tag_struct(CLIENT_BOOTSTRAP_NAME)
        .with_context(|| {
            format!(
                "Shadowkeep bootstrap {CLIENT_BOOTSTRAP_NAME} (0x{CLIENT_BOOTSTRAP_HASH:08X}, class 0x{CLIENT_BOOTSTRAP_CLASS:08X}) is unavailable"
            )
        })?;

    let globals = &bootstrap.render_globals;
    anyhow::ensure!(
        !globals.scopes.is_empty(),
        "Shadowkeep render globals class 0x{RENDER_GLOBALS_CLASS:08X} contains no scopes"
    );
    anyhow::ensure!(
        !globals.pipelines.is_empty(),
        "Shadowkeep render globals class 0x{RENDER_GLOBALS_CLASS:08X} contains no techniques"
    );
    anyhow::ensure!(
        !globals.input_layouts.mapping.layouts.is_empty(),
        "Shadowkeep input layout mapping class 0x808072A9 contains no layouts"
    );
    let scopes = globals
        .scopes
        .iter()
        .map(|scope| (scope.name.to_string(), scope.scope))
        .collect();
    let pipelines = globals
        .pipelines
        .iter()
        .map(|pipeline| (pipeline.name.to_string(), pipeline.technique))
        .collect();

    Ok(ShadowkeepRenderBootstrap {
        input_layouts: bootstrap.render_globals.input_layouts.taghash(),
        input_layout_count: bootstrap.render_globals.input_layouts.mapping.layouts.len(),
        scopes,
        pipelines,
        lookup_textures: bootstrap.render_globals.lookup_textures.taghash(),
        channel_defaults: bootstrap.render_globals.channel_defaults,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_extern_index_skips_only_the_post_bl_insertion() {
        assert_eq!(
            decode_extern_index(23),
            Some(ExternIndex::CuiScreenspaceBoxes)
        );
        assert_eq!(
            decode_extern_index(24),
            Some(ExternIndex::TextureVisualizer)
        );
        assert_eq!(decode_extern_index(96), Some(ExternIndex::SoftDeform));
        assert_eq!(decode_extern_index(97), None);
    }

    #[test]
    fn legacy_global_channels_are_positional_and_deterministic() {
        let channels = global_channel_defaults();
        assert_eq!(channels.len(), 256);
        assert_eq!(channels[10], Vec4::ZERO);
        assert_eq!(channels[27], Vec4::X);
        assert_eq!(channels[37], Vec4::X * 50.0);
        assert_eq!(channels[131], Vec4::new(0.5, 0.5, 0.3, 0.0));
    }

    #[test]
    fn profile_exposes_only_shadowkeep_renderer_ids() {
        let classes = ShadowkeepEraProfile.renderer_classes();
        assert_eq!(classes.client_bootstrap, CLIENT_BOOTSTRAP_CLASS);
        assert_eq!(classes.render_globals, RENDER_GLOBALS_CLASS);
        assert_eq!(classes.scope, SCOPE_CLASS);
        assert_eq!(classes.technique, TECHNIQUE_CLASS);
    }

    #[test]
    fn profile_exposes_shadowkeep_core_geometry_ids() {
        let classes = ShadowkeepEraProfile.geometry_classes();
        assert_eq!(classes.bubble_parent, 0x8080_7DAE);
        assert_eq!(classes.map_table, 0x8080_99D6);
        assert_eq!(classes.rigid_model_component, 0x8080_72B8);
        assert_eq!(classes.dynamic_model, 0x8080_73A5);
    }
}
