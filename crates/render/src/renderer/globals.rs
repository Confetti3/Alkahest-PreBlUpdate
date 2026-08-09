use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
    sync::Arc,
};

use alkahest_data::tfx::{
    features::cubemap::CubemapShape,
    render_globals::{SRenderGlobals, SRenderGlobalsData, SRenderGlobalsGlobalChannels},
    shadowkeep::{
        SShadowkeepLookupTextures, ShadowkeepRenderBootstrap, parse_shadowkeep_channel_defaults,
    },
};
use anyhow::Context;
use tiger_parse::PackageManagerExt;
use tiger_pkg::{TagHash, package_manager};

use crate::{
    Gpu,
    asset::{AssetManager, manager::TextureFallback, texture::Texture},
    tfx::{externs::get_global_channel_name, scope::Scope, technique::Technique},
};

fn load_shadowkeep_channel_defaults(tag: TagHash) -> anyhow::Result<GlobalChannels> {
    let manager = package_manager();
    let entry = manager
        .get_entry(tag)
        .with_context(|| format!("Shadowkeep channel-default tag {tag} has no package entry"))?;
    let bytes = manager
        .read_tag(tag)
        .with_context(|| format!("Failed to read Shadowkeep channel-default tag {tag}"))?;
    let parsed = parse_shadowkeep_channel_defaults(&bytes)
        .with_context(|| format!("Failed to decode Shadowkeep channel-default tag {tag}"))?;
    anyhow::ensure!(
        parsed.array_count <= 256,
        "Shadowkeep channel-default array has {} entries; the renderer ABI exposes 256 positional slots",
        parsed.array_count
    );
    let exact_named_channels = parsed
        .channel_hashes
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(index, hash)| {
            get_global_channel_name(hash).map(|name| (index, format!("0x{hash:08X}"), name))
        })
        .collect::<Vec<_>>();
    info!(
        tag = %tag,
        package_entry_reference = format_args!("0x{:08X}", entry.reference),
        package_entry_file_type = entry.file_type,
        package_entry_file_subtype = entry.file_subtype,
        package_entry_declared_size = entry.file_size,
        raw_byte_length = bytes.len(),
        declared_file_size = parsed.declared_file_size,
        header_size = parsed.header_size,
        hash_descriptor_offset = parsed.hash_descriptor_offset,
        value_descriptor_offset = parsed.value_descriptor_offset,
        auxiliary_descriptor_offset = parsed.auxiliary_descriptor_offset,
        hash_array_offset = parsed.hash_array_offset,
        value_array_offset = parsed.value_array_offset,
        auxiliary_array_offset = parsed.auxiliary_array_offset,
        array_count = parsed.array_count,
        auxiliary_count = parsed.auxiliary_count,
        hash_element_header = ?parsed.hash_element_header,
        value_element_header = ?parsed.value_element_header,
        auxiliary_element_header = ?parsed.auxiliary_element_header,
        channel_hashes = ?parsed.channel_hashes,
        exact_named_channels = ?exact_named_channels,
        candidate_vec4_values = ?parsed.values,
        auxiliary_fields = ?parsed.auxiliary_fields,
        interstitial_byte_length = parsed.interstitial_bytes.len(),
        trailing_byte_length = parsed.trailing_bytes.len(),
        "Decoded Shadowkeep positional channel defaults"
    );
    Ok(GlobalChannels {
        channel_ids: parsed.channel_hashes,
        default_values: parsed.values,
    })
}

pub struct RenderGlobals {
    pub scopes: GlobalScopes,
    pub pipelines: GlobalPipelines,

    pub textures: GlobalTextures,
    pub channels: GlobalChannels,
    // pub unk34: SUnk8080822d,
}

/// Shadowkeep serializes explicit null render-global entries.  They are
/// retained as unavailable slots so unrelated later-era globals cannot block
/// an otherwise valid legacy renderer startup.
pub struct ScopeSlot(Option<Box<Scope>>);

impl ScopeSlot {
    fn present(value: Scope) -> Self {
        Self(Some(Box::new(value)))
    }
    fn absent() -> Self {
        Self(None)
    }
    pub fn is_available(&self) -> bool {
        self.0.is_some()
    }
}

impl Deref for ScopeSlot {
    type Target = Scope;
    fn deref(&self) -> &Self::Target {
        self.0
            .as_deref()
            .expect("attempted to bind an unavailable Shadowkeep scope")
    }
}

impl DerefMut for ScopeSlot {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0
            .as_deref_mut()
            .expect("attempted to mutate an unavailable Shadowkeep scope")
    }
}

pub struct PipelineSlot(Option<Box<Technique>>);

impl PipelineSlot {
    fn present(value: Technique) -> Self {
        Self(Some(Box::new(value)))
    }
    fn absent() -> Self {
        Self(None)
    }
    pub fn is_available(&self) -> bool {
        self.0.is_some()
    }
}

impl Deref for PipelineSlot {
    type Target = Technique;
    fn deref(&self) -> &Self::Target {
        self.0
            .as_deref()
            .expect("attempted to bind an unavailable Shadowkeep pipeline")
    }
}

impl RenderGlobals {
    pub const CHANNEL_SUN_LIGHT_DIRECTION: u32 = 0x5C579DFA;

    pub fn load(gpu: &Arc<Gpu>, asset_manager: &AssetManager) -> anyhow::Result<Self> {
        let data: SRenderGlobals = package_manager().read_named_tag_struct("render_globals")?;
        let globs = &data.unk8.first().context("No render globals found")?.unk8.0;

        Ok(Self {
            scopes: GlobalScopes::load(gpu, asset_manager, globs),
            pipelines: GlobalPipelines::load(gpu, asset_manager, globs),
            textures: GlobalTextures::load(gpu, globs)?,
            channels: GlobalChannels::from(globs.global_channels.0.clone()),
            // unk34: globs.unk34.0.clone(),
        })
    }

    pub fn load_shadowkeep(
        gpu: &Arc<Gpu>,
        asset_manager: &AssetManager,
        bootstrap: &ShadowkeepRenderBootstrap,
    ) -> anyhow::Result<Self> {
        let channels = match load_shadowkeep_channel_defaults(bootstrap.channel_defaults) {
            Ok(channels) => channels,
            Err(error) => {
                warn!(
                    tag = %bootstrap.channel_defaults,
                    error = ?error,
                    "Shadowkeep package channel defaults could not be decoded; using degraded hand-authored positional fallback"
                );
                GlobalChannels {
                    channel_ids: Vec::new(),
                    default_values: alkahest_data::tfx::shadowkeep::global_channel_defaults()
                        .to_vec(),
                }
            }
        };
        Ok(Self {
            scopes: GlobalScopes::load_shadowkeep(gpu, asset_manager, bootstrap)?,
            pipelines: GlobalPipelines::load_shadowkeep(gpu, asset_manager, bootstrap)?,
            textures: GlobalTextures::load_shadowkeep(
                gpu,
                asset_manager,
                bootstrap.lookup_textures,
            )?,
            channels,
        })
    }
}

#[derive(Clone)]
pub struct GlobalChannels {
    pub channel_ids: Vec<u32>,
    pub default_values: Vec<glam::Vec4>,
}

impl From<SRenderGlobalsGlobalChannels> for GlobalChannels {
    fn from(value: SRenderGlobalsGlobalChannels) -> Self {
        Self {
            channel_ids: value.channel_ids,
            default_values: value.default_values,
        }
    }
}
pub struct GlobalTextures {
    pub specular_tint_lookup: Texture,
    pub specular_lobe_lookup: Texture,
    pub specular_lobe_3d_lookup: Texture,
    pub iridescence_lookup: Texture,
    pub water_displacement_unk00: Texture,
    pub water_displacement_unk08: Texture,
}

impl GlobalTextures {
    pub fn load(gpu: &Gpu, data: &SRenderGlobalsData) -> anyhow::Result<Self> {
        Ok(Self {
            specular_tint_lookup: Texture::load(gpu, data.unk30.specular_tint_lookup_texture)?,
            specular_lobe_lookup: Texture::load(gpu, data.unk30.specular_lobe_lookup_texture)?,
            specular_lobe_3d_lookup: Texture::load(
                gpu,
                data.unk30.specular_lobe_3d_lookup_texture,
            )?,
            iridescence_lookup: Texture::load(gpu, data.unk30.iridescence_lookup_texture)?,
            water_displacement_unk00: Texture::load(gpu, data.unk38.water_displacement_unk00)?,
            water_displacement_unk08: Texture::load(gpu, data.unk38.water_displacement_unk08)?,
        })
    }

    fn load_shadowkeep(
        gpu: &Gpu,
        asset_manager: &AssetManager,
        tag: TagHash,
    ) -> anyhow::Result<Self> {
        let data: SShadowkeepLookupTextures = package_manager().read_tag_struct(tag)?;
        Ok(Self {
            specular_tint_lookup: Self::load_shadowkeep_lookup(
                gpu,
                asset_manager,
                data.specular_tint_lookup_texture,
                "specular_tint_lookup",
            )?,
            specular_lobe_lookup: Self::load_shadowkeep_lookup(
                gpu,
                asset_manager,
                data.specular_lobe_lookup_texture,
                "specular_lobe_lookup",
            )?,
            specular_lobe_3d_lookup: Self::load_shadowkeep_lookup(
                gpu,
                asset_manager,
                data.specular_lobe_3d_lookup_texture,
                "specular_lobe_3d_lookup",
            )?,
            iridescence_lookup: Self::load_shadowkeep_lookup(
                gpu,
                asset_manager,
                data.iridescence_lookup_texture,
                "iridescence_lookup",
            )?,
            // These water slots are later-era globals. Do not attempt to
            // reinterpret a legacy texture as their serialized resource.
            water_displacement_unk00: Self::neutral_shadowkeep_lookup(
                gpu,
                "water_displacement_unk00",
            )?,
            water_displacement_unk08: Self::neutral_shadowkeep_lookup(
                gpu,
                "water_displacement_unk08",
            )?,
        })
    }

    fn load_shadowkeep_lookup(
        gpu: &Gpu,
        asset_manager: &AssetManager,
        tag: TagHash,
        name: &str,
    ) -> anyhow::Result<Texture> {
        match Texture::load_shadowkeep(gpu, tag) {
            Ok(texture) => Ok(texture),
            Err(error) => {
                warn!(%tag, %name, "Shadowkeep lookup texture could not be decoded; using neutral fallback: {error:#}");
                asset_manager.record_fallback(
                    tag,
                    TextureFallback::NeutralLookup,
                    format!("{name}: {error:#}"),
                );
                Self::neutral_shadowkeep_lookup(gpu, name)
            }
        }
    }

    fn neutral_shadowkeep_lookup(gpu: &Gpu, name: &str) -> anyhow::Result<Texture> {
        Texture::load_2d_raw(
            &gpu.device,
            1,
            1,
            &[128, 128, 128, 255],
            d3d11::dxgi::Format::R8g8b8a8Unorm,
            Some(&format!("Shadowkeep neutral lookup {name}")),
            false,
        )
    }
}

macro_rules! tfx_global_scopes {
    ($($name:ident),*) => {
        pub struct GlobalScopes {
            $(
                pub $name: ScopeSlot,
            )*
        }


        impl GlobalScopes {
            pub fn load(gpu: &Arc<Gpu>, asset_manager: &AssetManager, globals: &SRenderGlobalsData) -> Self {
                let scopes: HashMap<String, TagHash> = globals.scopes.iter().map(|p| (p.name.to_string(), p.scope)).collect();

                Self {
                    $(
                        $name: ScopeSlot::present(Scope::load(
                            gpu, asset_manager,
                            *scopes.get(stringify!($name))
                                .expect(&format!("Scope {} does not exist", stringify!($name))),
                        )
                        .expect("Failed to load scope")),
                    )*
                }
            }

            fn load_shadowkeep(gpu: &Arc<Gpu>, asset_manager: &AssetManager, bootstrap: &ShadowkeepRenderBootstrap) -> anyhow::Result<Self> {
                Ok(Self {
                    $($name: match bootstrap.scopes.get(stringify!($name)).copied().filter(|tag| tag.is_some()) {
                        Some(tag) => ScopeSlot::present(Scope::load_shadowkeep(
                            gpu, asset_manager, tag,
                        ).with_context(|| format!("Failed to load Shadowkeep scope {}", stringify!($name)))?),
                        None => ScopeSlot::absent(),
                    },)*
                })
            }
        }
    };
}

tfx_global_scopes! {
    frame, view, rigid_model, editor_mesh, editor_terrain,
    cui_view, cui_object, skinning, speedtree, chunk_model,
    decal, instances, speedtree_lod_drawcall_data, transparent,
    transparent_advanced, sdsm_bias_and_scale_textures, terrain,
    postprocess, cui_bitmap, cui_standard, ui_font, cui_hud,
    particle_transforms, particle_location_metadata, cubemap_volume,
    gear_plated_textures, generic_array
}

macro_rules! tfx_global_pipelines {
    ($($name:ident),*) => {
        #[allow(non_snake_case)]
        pub struct GlobalPipelines {
            $(
                pub $name: PipelineSlot,
            )*
        }


        impl GlobalPipelines {
            pub fn load(gpu: &Arc<Gpu>, asset_manager: &AssetManager, globals: &SRenderGlobalsData) -> Self {
                let techniques: HashMap<String, TagHash> = globals.pipelines.iter().map(|p| (p.name.to_string(), p.technique)).collect();

                Self {
                    $(
                        $name: PipelineSlot::present(
                            Technique::load(
                            gpu, asset_manager,
                                *techniques.get(stringify!($name))
                                    .expect(&format!("Technique {} does not exist", stringify!($name)))
                            )
                            .unwrap_or_else(|e| panic!("Failed to read global pipeline technique {}: {e:?}", stringify!($name))),
                        ),
                    )*
                }
            }

            fn load_shadowkeep(gpu: &Arc<Gpu>, asset_manager: &AssetManager, bootstrap: &ShadowkeepRenderBootstrap) -> anyhow::Result<Self> {
                Ok(Self {
                    $($name: match bootstrap.pipelines.get(stringify!($name)).copied().filter(|tag| tag.is_some()) {
                        Some(tag) => PipelineSlot::present(Technique::load_shadowkeep(
                            gpu, asset_manager, tag,
                        ).with_context(|| format!("Failed to load Shadowkeep pipeline {}", stringify!($name)))?),
                        None => PipelineSlot::absent(),
                    },)*
                })
            }
        }
    };
}

tfx_global_pipelines! {
    // Shading
    clear_color_2_mrt,
    deferred_shading,
    deferred_shading_no_atm,
    global_lighting,
    global_lighting_and_shading,
    global_lighting_and_shading_gel,
    final_combine_no_film_curve,
    final_combine,

    // Post
    hdao,
    apply_ssao_to_light_buffers,
    ssao_bilateral_filter,
    // ssao_compute_ao_3D_ps,
    fxaa,
    fxaa_noise,
    autoexposure_sample_columns,

    // Utility
    copy_texture_bilinear,
    copy_texture_bilinear_tiled,

    // Cubemap variants
    cubemap_apply_cube_alpha_off_probes_off_relighting_off, cubemap_apply_cube_alpha_off_probes_off_relighting_on,
    cubemap_apply_cube_alpha_off_probes_on_relighting_off, cubemap_apply_cube_alpha_off_probes_on_relighting_on,
    cubemap_apply_cube_alpha_on_probes_off_relighting_off, cubemap_apply_cube_alpha_on_probes_off_relighting_on,
    cubemap_apply_cube_alpha_on_probes_on_relighting_off, cubemap_apply_cube_alpha_on_probes_on_relighting_on,
    cubemap_apply_cube_sphere_alpha_off_probes_off_relighting_off, cubemap_apply_cube_sphere_alpha_off_probes_off_relighting_on,
    cubemap_apply_cube_sphere_alpha_off_probes_on_relighting_off, cubemap_apply_cube_sphere_alpha_off_probes_on_relighting_on,
    cubemap_apply_cube_sphere_alpha_on_probes_off_relighting_off, cubemap_apply_cube_sphere_alpha_on_probes_off_relighting_on,
    cubemap_apply_cube_sphere_alpha_on_probes_on_relighting_off, cubemap_apply_cube_sphere_alpha_on_probes_on_relighting_on,
    cubemap_apply_sphere_alpha_off_probes_off_relighting_off, cubemap_apply_sphere_alpha_off_probes_off_relighting_on,
    cubemap_apply_sphere_alpha_off_probes_on_relighting_off, cubemap_apply_sphere_alpha_off_probes_on_relighting_on,
    cubemap_apply_sphere_alpha_on_probes_off_relighting_off, cubemap_apply_sphere_alpha_on_probes_off_relighting_on,
    cubemap_apply_sphere_alpha_on_probes_on_relighting_off, cubemap_apply_sphere_alpha_on_probes_on_relighting_on,
    cubemap_apply_parall_cube_alpha_off_probes_off_relighting_off, cubemap_apply_parall_cube_alpha_off_probes_off_relighting_on,
    cubemap_apply_parall_cube_alpha_off_probes_on_relighting_off, cubemap_apply_parall_cube_alpha_off_probes_on_relighting_on,
    cubemap_apply_parall_cube_alpha_on_probes_off_relighting_off, cubemap_apply_parall_cube_alpha_on_probes_off_relighting_on,
    cubemap_apply_parall_cube_alpha_on_probes_on_relighting_off, cubemap_apply_parall_cube_alpha_on_probes_on_relighting_on,
    cubemap_apply_parall_cube_sphere_alpha_off_probes_off_relighting_off, cubemap_apply_parall_cube_sphere_alpha_off_probes_off_relighting_on,
    cubemap_apply_parall_cube_sphere_alpha_off_probes_on_relighting_off, cubemap_apply_parall_cube_sphere_alpha_off_probes_on_relighting_on,
    cubemap_apply_parall_cube_sphere_alpha_on_probes_off_relighting_off, cubemap_apply_parall_cube_sphere_alpha_on_probes_off_relighting_on,
    cubemap_apply_parall_cube_sphere_alpha_on_probes_on_relighting_off, cubemap_apply_parall_cube_sphere_alpha_on_probes_on_relighting_on,
    cubemap_apply_parall_sphere_alpha_off_probes_off_relighting_off, cubemap_apply_parall_sphere_alpha_off_probes_off_relighting_on,
    cubemap_apply_parall_sphere_alpha_off_probes_on_relighting_off, cubemap_apply_parall_sphere_alpha_off_probes_on_relighting_on,
    cubemap_apply_parall_sphere_alpha_on_probes_off_relighting_off, cubemap_apply_parall_sphere_alpha_on_probes_off_relighting_on,
    cubemap_apply_parall_sphere_alpha_on_probes_on_relighting_off, cubemap_apply_parall_sphere_alpha_on_probes_on_relighting_on,

    cubemap_apply_sky_copy_ao,
    sky_generate_sky_mask,
    sky_lookup_generate_near, sky_lookup_generate_far,
    sky,

    debug_cubemap_diffuse_probes,
    debug_source_color,
    debug_specular_smoothness,
    debug_metalness,
    debug_texture_ao,
    debug_ambient_occlusion,
    debug_emissive,
    debug_emissive_intensity,
    debug_transmission,
    debug_colored_overcoat_id,
    debug_depth_edges,
    debug_world_normal,
    debug_diffuse_light,
    debug_specular_light,
    overdraw_visualizer,

    // LUT3D variants
    screen_area_global_lut3d_distort,
    screen_area_global_lut3d_distort_hdr,
    screen_area_global_lut3d_distort_noise,
    screen_area_global_lut3d_distort_noise_hdr,
    screen_area_global_lut3d,
    screen_area_global_lut3d_hdr,
    screen_area_global_lut3d_noise,
    screen_area_global_lut3d_noise_hdr,
    screen_area_global_lut3d_no_tonemap,

    downsample_depth_buffer, uber_depth_default, downsample_max_min_avg_no_swizzle,

    bloom_initial_downsample_block_2x2, downsample_block_2x2_with_nan_kill, downsample_block_2x2,
    downsample_gaussian_1x8, downsample_gaussian_8x1,
    downsample_gaussian_1x16, downsample_gaussian_16x1,

    gaussian_10_horz, gaussian_10_vert,
    weighted_6_horz, weighted_6_vert,
    combined_bloom_line_blur,
    radial_blur_8,

    weighted_add,

    volumetrics_upres_1,

    water_sky_color_generate,
    water_reflection_healing,
    water_reflection_resolve,
    water_reflection_uv_healing
}

impl GlobalPipelines {
    pub fn get_specialized_cubemap_pipeline(
        &self,
        shape: CubemapShape,
        alpha: bool,
        probes: bool,
        relighting: bool,
        parallax: bool,
    ) -> &Technique {
        let pipeline_list = [
            // No Parallax
            [
                // Cube
                [
                    // Alpha Off
                    [
                        // Probes Off
                        [
                            &self.cubemap_apply_cube_alpha_off_probes_off_relighting_off,
                            &self.cubemap_apply_cube_alpha_off_probes_off_relighting_on,
                        ],
                        // Probes On
                        [
                            &self.cubemap_apply_cube_alpha_off_probes_on_relighting_off,
                            &self.cubemap_apply_cube_alpha_off_probes_on_relighting_on,
                        ],
                    ],
                    // Alpha On
                    [
                        // Probes Off
                        [
                            &self.cubemap_apply_cube_alpha_on_probes_off_relighting_off,
                            &self.cubemap_apply_cube_alpha_on_probes_off_relighting_on,
                        ],
                        // Probes On
                        [
                            &self.cubemap_apply_cube_alpha_on_probes_on_relighting_off,
                            &self.cubemap_apply_cube_alpha_on_probes_on_relighting_on,
                        ],
                    ],
                ],
                // Sphere
                [
                    // Alpha Off
                    [
                        // Probes Off
                        [
                            &self.cubemap_apply_sphere_alpha_off_probes_off_relighting_off,
                            &self.cubemap_apply_sphere_alpha_off_probes_off_relighting_on,
                        ],
                        // Probes On
                        [
                            &self.cubemap_apply_sphere_alpha_off_probes_on_relighting_off,
                            &self.cubemap_apply_sphere_alpha_off_probes_on_relighting_on,
                        ],
                    ],
                    // Alpha On
                    [
                        // Probes Off
                        [
                            &self.cubemap_apply_sphere_alpha_on_probes_off_relighting_off,
                            &self.cubemap_apply_sphere_alpha_on_probes_off_relighting_on,
                        ],
                        // Probes On
                        [
                            &self.cubemap_apply_sphere_alpha_on_probes_on_relighting_off,
                            &self.cubemap_apply_sphere_alpha_on_probes_on_relighting_on,
                        ],
                    ],
                ],
                // CubeSphere
                [
                    // Alpha Off
                    [
                        // Probes Off
                        [
                            &self.cubemap_apply_cube_sphere_alpha_off_probes_off_relighting_off,
                            &self.cubemap_apply_cube_sphere_alpha_off_probes_off_relighting_on,
                        ],
                        // Probes On
                        [
                            &self.cubemap_apply_cube_sphere_alpha_off_probes_on_relighting_off,
                            &self.cubemap_apply_cube_sphere_alpha_off_probes_on_relighting_on,
                        ],
                    ],
                    // Alpha On
                    [
                        // Probes Off
                        [
                            &self.cubemap_apply_cube_sphere_alpha_on_probes_off_relighting_off,
                            &self.cubemap_apply_cube_sphere_alpha_on_probes_off_relighting_on,
                        ],
                        // Probes On
                        [
                            &self.cubemap_apply_cube_sphere_alpha_on_probes_on_relighting_off,
                            &self.cubemap_apply_cube_sphere_alpha_on_probes_on_relighting_on,
                        ],
                    ],
                ],
            ],
            // Parallax
            [
                // ParallCube
                [
                    // Alpha Off
                    [
                        // Probes Off
                        [
                            &self.cubemap_apply_parall_cube_alpha_off_probes_off_relighting_off,
                            &self.cubemap_apply_parall_cube_alpha_off_probes_off_relighting_on,
                        ],
                        // Probes On
                        [
                            &self.cubemap_apply_parall_cube_alpha_off_probes_on_relighting_off,
                            &self.cubemap_apply_parall_cube_alpha_off_probes_on_relighting_on,
                        ],
                    ],
                    // Alpha On
                    [
                        // Probes Off
                        [
                            &self.cubemap_apply_parall_cube_alpha_on_probes_off_relighting_off,
                            &self.cubemap_apply_parall_cube_alpha_on_probes_off_relighting_on,
                        ],
                        // Probes On
                        [
                            &self.cubemap_apply_parall_cube_alpha_on_probes_on_relighting_off,
                            &self.cubemap_apply_parall_cube_alpha_on_probes_on_relighting_on,
                        ],
                    ],
                ],
                // ParallSphere
                [
                    // Alpha Off
                    [
                        // Probes Off
                        [
                            &self.cubemap_apply_parall_sphere_alpha_off_probes_off_relighting_off,
                            &self.cubemap_apply_parall_sphere_alpha_off_probes_off_relighting_on,
                        ],
                        // Probes On
                        [
                            &self.cubemap_apply_parall_sphere_alpha_off_probes_on_relighting_off,
                            &self.cubemap_apply_parall_sphere_alpha_off_probes_on_relighting_on,
                        ],
                    ],
                    // Alpha On
                    [
                        // Probes Off
                        [
                            &self.cubemap_apply_parall_sphere_alpha_on_probes_off_relighting_off,
                            &self.cubemap_apply_parall_sphere_alpha_on_probes_off_relighting_on,
                        ],
                        // Probes On
                        [
                            &self.cubemap_apply_parall_sphere_alpha_on_probes_on_relighting_off,
                            &self.cubemap_apply_parall_sphere_alpha_on_probes_on_relighting_on,
                        ],
                    ],
                ],
                // ParallCubeSphere
                [
                    // Alpha Off
                    [
                        // Probes Off
                        [
                            &self.cubemap_apply_parall_cube_sphere_alpha_off_probes_off_relighting_off,
                            &self.cubemap_apply_parall_cube_sphere_alpha_off_probes_off_relighting_on,
                        ],
                        // Probes On
                        [
                            &self.cubemap_apply_parall_cube_sphere_alpha_off_probes_on_relighting_off,
                            &self.cubemap_apply_parall_cube_sphere_alpha_off_probes_on_relighting_on,
                        ],
                    ],
                    // Alpha On
                    [
                        // Probes Off
                        [
                            &self.cubemap_apply_parall_cube_sphere_alpha_on_probes_off_relighting_off,
                            &self.cubemap_apply_parall_cube_sphere_alpha_on_probes_off_relighting_on,
                        ],
                        // Probes On
                        [
                            &self.cubemap_apply_parall_cube_sphere_alpha_on_probes_on_relighting_off,
                            &self.cubemap_apply_parall_cube_sphere_alpha_on_probes_on_relighting_on,
                        ],
                    ],
                ],
            ],
        ];

        pipeline_list[parallax as usize][shape as usize][alpha as usize][probes as usize]
            [relighting as usize]
    }

    pub fn get_specialized_lut3d_pipeline(
        &self,
        distort: bool,
        hdr: bool,
        noise: bool,
    ) -> &Technique {
        let pipeline_list = [
            // No Distort
            [
                // No Noise
                [
                    &self.screen_area_global_lut3d,
                    &self.screen_area_global_lut3d_hdr,
                ],
                // Noise
                [
                    &self.screen_area_global_lut3d_noise,
                    &self.screen_area_global_lut3d_noise_hdr,
                ],
            ],
            // Distort
            [
                // No Noise
                [
                    &self.screen_area_global_lut3d_distort,
                    &self.screen_area_global_lut3d_distort_hdr,
                ],
                // Noise
                [
                    &self.screen_area_global_lut3d_distort_noise,
                    &self.screen_area_global_lut3d_distort_noise_hdr,
                ],
            ],
        ];

        pipeline_list[distort as usize][noise as usize][hdr as usize]
    }
}
