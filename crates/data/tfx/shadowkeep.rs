//! Shadowkeep/Season of Arrivals render bootstrap layouts.
//!
//! These deliberately live beside, rather than replacing, the later-era
//! `render_globals` layouts.  A matching Rust name is not evidence of a
//! matching on-disk structure.

use std::collections::HashMap;

use anyhow::{ensure, Context};
use glam::Vec4;
use tiger_parse::{
    tiger_type, Endian, NullString, PackageManagerExt, Pointer, TigerReadable, TigerReader,
};
use tiger_pkg::{package_manager, TagHash};

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

/// The Arrivals channel-default tag contains three Tiger vectors after its
/// file-size field: 153 channel hashes, 153 positional Vec4 defaults, and a
/// five-u32 auxiliary array. Each descriptor is a u64 count plus a signed
/// pointer relative to the pointer field itself. Each target repeats the count
/// and carries an eight-byte element header before its payload.
#[derive(Debug, Clone, PartialEq)]
pub struct ShadowkeepChannelDefaults {
    pub declared_file_size: u64,
    pub header_size: usize,
    pub hash_descriptor_offset: usize,
    pub value_descriptor_offset: usize,
    pub auxiliary_descriptor_offset: usize,
    pub hash_array_offset: usize,
    pub value_array_offset: usize,
    pub auxiliary_array_offset: usize,
    pub array_count: usize,
    pub auxiliary_count: usize,
    pub hash_element_header: [u8; 8],
    pub value_element_header: [u8; 8],
    pub auxiliary_element_header: [u8; 8],
    pub channel_hashes: Vec<u32>,
    pub values: Vec<Vec4>,
    pub auxiliary_fields: Vec<u32>,
    pub interstitial_bytes: Vec<u8>,
    pub trailing_bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ShadowkeepChannelDefaultsLoad {
    Package(ShadowkeepChannelDefaults),
    Fallback { reason: String },
}

fn read_u64(bytes: &[u8], offset: usize, label: &str) -> anyhow::Result<u64> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| anyhow::anyhow!("{label} offset overflow"))?;
    let value = bytes
        .get(offset..end)
        .ok_or_else(|| anyhow::anyhow!("truncated {label} at 0x{offset:X}"))?;
    Ok(u64::from_le_bytes(value.try_into().unwrap()))
}

fn read_i64(bytes: &[u8], offset: usize, label: &str) -> anyhow::Result<i64> {
    Ok(read_u64(bytes, offset, label)? as i64)
}

fn relative_offset(base: usize, offset: i64, label: &str) -> anyhow::Result<usize> {
    if offset >= 0 {
        base.checked_add(offset as usize)
    } else {
        base.checked_sub(offset.unsigned_abs() as usize)
    }
    .ok_or_else(|| anyhow::anyhow!("{label} points outside addressable range"))
}

fn parse_vector_layout(
    bytes: &[u8],
    count_offset: usize,
    pointer_offset: usize,
    element_size: usize,
    label: &str,
) -> anyhow::Result<(usize, usize, usize, [u8; 8])> {
    const TARGET_HEADER_SIZE: usize = 0x10;

    let count_u64 = read_u64(bytes, count_offset, &format!("{label} count"))?;
    ensure!(count_u64 > 0, "{label} count must be positive");
    let count =
        usize::try_from(count_u64).map_err(|_| anyhow::anyhow!("{label} count is too large"))?;
    let relative = read_i64(bytes, pointer_offset, &format!("{label} pointer"))?;
    let target = relative_offset(pointer_offset, relative, label)?;
    let payload_start = target
        .checked_add(TARGET_HEADER_SIZE)
        .ok_or_else(|| anyhow::anyhow!("{label} target header overflows"))?;
    ensure!(
        payload_start <= bytes.len(),
        "{label} target header at 0x{target:X} is truncated"
    );
    let target_count = read_u64(bytes, target, &format!("{label} target count"))?;
    ensure!(
        target_count == count_u64,
        "{label} descriptor count {count_u64} disagrees with target count {target_count}"
    );
    let payload_len = count
        .checked_mul(element_size)
        .ok_or_else(|| anyhow::anyhow!("{label} payload size overflows"))?;
    let payload_end = payload_start
        .checked_add(payload_len)
        .ok_or_else(|| anyhow::anyhow!("{label} payload end overflows"))?;
    ensure!(
        payload_end <= bytes.len(),
        "{label} payload is truncated: need {payload_len} bytes at 0x{payload_start:X}"
    );
    let element_header = bytes[target + 8..target + TARGET_HEADER_SIZE]
        .try_into()
        .unwrap();
    Ok((count, target, payload_end, element_header))
}

/// Decode the package payload after the channel-default tag has been
/// resolved. This remains independent of package-manager state so malformed
/// and truncated payloads are directly testable.
pub fn parse_shadowkeep_channel_defaults(
    bytes: &[u8],
) -> anyhow::Result<ShadowkeepChannelDefaults> {
    const ROOT_HEADER_SIZE: usize = 0x38;
    const HASH_COUNT_OFFSET: usize = 0x08;
    const HASH_POINTER_OFFSET: usize = 0x10;
    const VALUE_COUNT_OFFSET: usize = 0x18;
    const VALUE_POINTER_OFFSET: usize = 0x20;
    const AUXILIARY_COUNT_OFFSET: usize = 0x28;
    const AUXILIARY_POINTER_OFFSET: usize = 0x30;

    ensure!(
        bytes.len() >= ROOT_HEADER_SIZE,
        "channel-default resource is truncated before its three vector descriptors"
    );
    let declared_file_size = read_u64(bytes, 0, "channel-default file size")?;
    ensure!(
        declared_file_size == bytes.len() as u64,
        "channel-default file-size header declares {declared_file_size} bytes, payload has {}",
        bytes.len()
    );

    let (hash_count, hash_array_offset, hash_end, hash_element_header) = parse_vector_layout(
        bytes,
        HASH_COUNT_OFFSET,
        HASH_POINTER_OFFSET,
        4,
        "channel hash vector",
    )?;
    let (value_count, value_array_offset, value_end, value_element_header) = parse_vector_layout(
        bytes,
        VALUE_COUNT_OFFSET,
        VALUE_POINTER_OFFSET,
        16,
        "channel value vector",
    )?;
    let (auxiliary_count, auxiliary_array_offset, auxiliary_end, auxiliary_element_header) =
        parse_vector_layout(
            bytes,
            AUXILIARY_COUNT_OFFSET,
            AUXILIARY_POINTER_OFFSET,
            4,
            "channel auxiliary vector",
        )?;
    ensure!(
        hash_count == value_count,
        "channel hash count {hash_count} disagrees with positional value count {value_count}"
    );
    ensure!(
        ROOT_HEADER_SIZE <= hash_array_offset
            && hash_end <= value_array_offset
            && value_end <= auxiliary_array_offset,
        "channel-default vectors overlap or are out of package order"
    );

    let hash_start = hash_array_offset + 0x10;
    let value_start = value_array_offset + 0x10;
    let auxiliary_start = auxiliary_array_offset + 0x10;
    let channel_hashes = bytes[hash_start..hash_end]
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
        .collect();
    let values = bytes[value_start..value_end]
        .chunks_exact(16)
        .map(|chunk| {
            Vec4::new(
                f32::from_le_bytes(chunk[0..4].try_into().unwrap()),
                f32::from_le_bytes(chunk[4..8].try_into().unwrap()),
                f32::from_le_bytes(chunk[8..12].try_into().unwrap()),
                f32::from_le_bytes(chunk[12..16].try_into().unwrap()),
            )
        })
        .collect();
    let auxiliary_fields = bytes[auxiliary_start..auxiliary_end]
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
        .collect();
    let mut interstitial_bytes = Vec::new();
    interstitial_bytes.extend_from_slice(&bytes[ROOT_HEADER_SIZE..hash_array_offset]);
    interstitial_bytes.extend_from_slice(&bytes[hash_end..value_array_offset]);
    interstitial_bytes.extend_from_slice(&bytes[value_end..auxiliary_array_offset]);

    Ok(ShadowkeepChannelDefaults {
        declared_file_size,
        header_size: ROOT_HEADER_SIZE,
        hash_descriptor_offset: HASH_COUNT_OFFSET,
        value_descriptor_offset: VALUE_COUNT_OFFSET,
        auxiliary_descriptor_offset: AUXILIARY_COUNT_OFFSET,
        hash_array_offset,
        value_array_offset,
        auxiliary_array_offset,
        array_count: value_count,
        auxiliary_count,
        hash_element_header,
        value_element_header,
        auxiliary_element_header,
        channel_hashes,
        values,
        auxiliary_fields,
        interstitial_bytes,
        trailing_bytes: bytes[auxiliary_end..].to_vec(),
    })
}

/// Preserve the hand-authored table only as an explicit degraded fallback.
pub fn shadowkeep_channel_defaults_with_fallback(
    bytes: &[u8],
) -> (Vec<Vec4>, ShadowkeepChannelDefaultsLoad) {
    match parse_shadowkeep_channel_defaults(bytes) {
        Ok(parsed) => (
            parsed.values.clone(),
            ShadowkeepChannelDefaultsLoad::Package(parsed),
        ),
        Err(error) => {
            let reason = format!("{error:#}");
            (
                global_channel_defaults().to_vec(),
                ShadowkeepChannelDefaultsLoad::Fallback { reason },
            )
        }
    }
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

    fn encoded_channel_defaults(values: &[Vec4], trailing: &[u8]) -> Vec<u8> {
        fn write_descriptor(
            bytes: &mut [u8],
            count_offset: usize,
            pointer_offset: usize,
            count: usize,
            target: usize,
        ) {
            bytes[count_offset..count_offset + 8].copy_from_slice(&(count as u64).to_le_bytes());
            bytes[pointer_offset..pointer_offset + 8]
                .copy_from_slice(&((target - pointer_offset) as i64).to_le_bytes());
        }

        let auxiliary: [u32; 5] = [0, 0, 0, 0x74, 0];
        let hash_target = 0x38;
        let value_target = hash_target + 0x10 + values.len() * 4;
        let auxiliary_target = value_target + 0x10 + values.len() * 16;
        let payload_end = auxiliary_target + 0x10 + auxiliary.len() * 4;
        let mut bytes = vec![0; payload_end + trailing.len()];
        let byte_len = bytes.len() as u64;
        bytes[0..8].copy_from_slice(&byte_len.to_le_bytes());
        write_descriptor(&mut bytes, 0x08, 0x10, values.len(), hash_target);
        write_descriptor(&mut bytes, 0x18, 0x20, values.len(), value_target);
        write_descriptor(&mut bytes, 0x28, 0x30, auxiliary.len(), auxiliary_target);

        for (target, count, header) in [
            (
                hash_target,
                values.len(),
                [0x70, 0x00, 0x80, 0x80, 0, 0, 0, 0],
            ),
            (
                value_target,
                values.len(),
                [0x90, 0x00, 0x80, 0x80, 0, 0, 0, 0],
            ),
            (
                auxiliary_target,
                auxiliary.len(),
                [0x0b, 0x00, 0x80, 0x80, 0, 0, 0, 0],
            ),
        ] {
            bytes[target..target + 8].copy_from_slice(&(count as u64).to_le_bytes());
            bytes[target + 8..target + 0x10].copy_from_slice(&header);
        }
        for index in 0..values.len() {
            let start = hash_target + 0x10 + index * 4;
            bytes[start..start + 4].copy_from_slice(&(index as u32).to_le_bytes());
        }
        for (index, value) in values.iter().enumerate() {
            let start = value_target + 0x10 + index * 16;
            for (component, scalar) in value.to_array().iter().enumerate() {
                bytes[start + component * 4..start + component * 4 + 4]
                    .copy_from_slice(&scalar.to_le_bytes());
            }
        }
        for (index, value) in auxiliary.iter().enumerate() {
            let start = auxiliary_target + 0x10 + index * 4;
            bytes[start..start + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes[payload_end..].copy_from_slice(trailing);
        bytes
    }

    #[test]
    fn channel_defaults_report_the_encoded_array_count() {
        let values = [Vec4::X, Vec4::Y, Vec4::Z];
        let parsed =
            parse_shadowkeep_channel_defaults(&encoded_channel_defaults(&values, &[])).unwrap();
        assert_eq!(parsed.array_count, values.len());
        assert_eq!(parsed.channel_hashes, [0, 1, 2]);
        assert_eq!(parsed.values, values);
        assert_eq!(parsed.auxiliary_fields, [0, 0, 0, 0x74, 0]);
        assert_eq!(
            parsed.value_element_header,
            [0x90, 0x00, 0x80, 0x80, 0, 0, 0, 0]
        );
    }

    #[test]
    fn truncated_channel_defaults_are_rejected() {
        let mut bytes = encoded_channel_defaults(&[Vec4::ONE], &[]);
        bytes.pop();
        let error = parse_shadowkeep_channel_defaults(&bytes).unwrap_err();
        assert!(error.to_string().contains("declares"));
    }

    #[test]
    fn channel_default_values_remain_positional() {
        let values = [
            Vec4::new(2.0, 3.0, 5.0, 7.0),
            Vec4::new(11.0, 13.0, 17.0, 19.0),
        ];
        let parsed =
            parse_shadowkeep_channel_defaults(&encoded_channel_defaults(&values, &[])).unwrap();
        assert_eq!(parsed.values[0], values[0]);
        assert_eq!(parsed.values[1], values[1]);
    }

    #[test]
    fn malformed_channel_defaults_expose_the_degraded_fallback() {
        let (values, source) = shadowkeep_channel_defaults_with_fallback(&[0; 7]);
        assert_eq!(values, global_channel_defaults().to_vec());
        match source {
            ShadowkeepChannelDefaultsLoad::Fallback { reason } => {
                assert!(reason.contains("channel-default resource"));
            }
            ShadowkeepChannelDefaultsLoad::Package(_) => panic!("malformed data decoded"),
        }
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
