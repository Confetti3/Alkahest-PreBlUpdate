//! Shadowkeep renderer resources that are intentionally owned by the renderer
//! rather than the package-independent D3D platform.

use std::{collections::HashMap, sync::Arc};

use alkahest_data::tfx::shadowkeep::{
    SShadowkeepVertexInputLayouts, ShadowkeepEraProfile, ShadowkeepRenderBootstrap,
};
use anyhow::{Context, ensure};
use d3d11::DeviceChild;
use tiger_parse::PackageManagerExt;
use tiger_pkg::{TagHash, package_manager};

use crate::{
    Gpu,
    asset::AssetManager,
    gpu::global_state::{INPUT_FORMATS, INPUT_SEMANTICS, RenderStates, TigerInputLayoutElement},
    renderer::globals::GlobalPipelines,
    tfx::{scope::Scope, technique::Technique},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityState {
    Ready,
    Degraded,
    Blocked,
    AbsentInCorpus,
}

#[derive(Debug, Clone)]
pub struct CapabilityRecord {
    pub name: &'static str,
    pub state: CapabilityState,
    pub evidence: String,
}

/// Conservative renderer capability ledger.  `AbsentInCorpus` is reserved
/// for a completed corpus-family scan; this bootstrap stage therefore never
/// assigns it speculatively.
pub fn bootstrap_capability_ledger() -> Vec<CapabilityRecord> {
    vec![
        CapabilityRecord {
            name: "Shadowkeep bootstrap and input layouts",
            state: CapabilityState::Ready,
            evidence: "client_bootstrap_patchable, render globals, and all dynamic layout records parsed".into(),
        },
        CapabilityRecord {
            name: "TFX scopes, techniques, and positional channels",
            state: CapabilityState::Ready,
            evidence: "legacy serializers and extern-index translation are active".into(),
        },
        CapabilityRecord {
            name: "Core geometry submission",
            state: CapabilityState::Degraded,
            evidence: "static placements, terrain patches, and map-contained rigid models normalize into the modern submitter; visual-family parity is still incomplete".into(),
        },
        CapabilityRecord {
            name: "Lighting, atmosphere, shadows, cubemaps, and post-processing",
            state: CapabilityState::Blocked,
            evidence: "requires the core Shadowkeep geometry and era-specific pass graph".into(),
        },
        CapabilityRecord {
            name: "Water, decals, distortion, volumetrics, and autoexposure",
            state: CapabilityState::Blocked,
            evidence: "no corpus-absence claim has been made".into(),
        },
    ]
}

/// Metadata-first catalog.  It lets the application report exactly which
/// legacy resource blocks a feature without eagerly compiling the complete
/// 623-technique bootstrap.
pub struct ShadowkeepTechniqueRegistry {
    gpu: Arc<Gpu>,
    asset_manager: AssetManager,
    scopes: HashMap<String, TagHash>,
    techniques: HashMap<String, TagHash>,
}

impl ShadowkeepTechniqueRegistry {
    pub fn new(gpu: Arc<Gpu>, asset_manager: AssetManager, bootstrap: &ShadowkeepRenderBootstrap) -> Self {
        Self {
            gpu,
            asset_manager,
            scopes: bootstrap.scopes.clone(),
            techniques: bootstrap.pipelines.clone(),
        }
    }

    pub fn load_scope(&self, name: &str) -> anyhow::Result<Scope> {
        let tag = *self
            .scopes
            .get(name)
            .with_context(|| format!("Shadowkeep render globals has no scope named {name}"))?;
        ensure!(tag.is_some(), "Shadowkeep scope {name} is explicitly null");
        Scope::load_shadowkeep(&self.gpu, &self.asset_manager, tag)
            .with_context(|| format!("Failed to load Shadowkeep scope {name} ({tag})"))
    }

    pub fn load_technique(&self, name: &str) -> anyhow::Result<Technique> {
        let tag = *self
            .techniques
            .get(name)
            .with_context(|| format!("Shadowkeep render globals has no technique named {name}"))?;
        ensure!(tag.is_some(), "Shadowkeep technique {name} is explicitly null");
        Technique::load_shadowkeep(&self.gpu, &self.asset_manager, tag)
            .with_context(|| format!("Failed to load Shadowkeep technique {name} ({tag})"))
    }

    pub fn has_scope(&self, name: &str) -> bool {
        self.scopes.get(name).is_some_and(|tag| tag.is_some())
    }

    pub fn has_technique(&self, name: &str) -> bool {
        self.techniques.get(name).is_some_and(|tag| tag.is_some())
    }
}

/// Dynamic vertex layouts for the preserved format.  They are never stored in
/// `Gpu`; callers retain this table with the era renderer that created it.
pub struct ShadowkeepInputLayouts {
    pub layouts: Vec<Option<d3d11::InputLayout>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowkeepPassState {
    Ready,
    Degraded,
    DisabledAsAbsent,
    Failed,
}

#[derive(Debug, Clone)]
pub struct ShadowkeepPassRecord {
    pub name: &'static str,
    pub state: ShadowkeepPassState,
    pub evidence: &'static str,
}

/// Runtime pass ledger for the admitted Shadowkeep path.  A present legacy
/// technique is not promoted to parity merely because it binds successfully:
/// the fullscreen lighting stages remain degraded until a non-trivial capture
/// proves their output.  Explicitly null bootstrap entries are reported as
/// absent instead of being filled with a post-BL substitute.
pub fn pass_status_ledger(pipelines: &GlobalPipelines) -> Vec<ShadowkeepPassRecord> {
    let lighting_state = if pipelines.global_lighting.is_available() {
        ShadowkeepPassState::Degraded
    } else {
        ShadowkeepPassState::DisabledAsAbsent
    };
    let deferred_state = if pipelines.deferred_shading_no_atm.is_available() {
        ShadowkeepPassState::Degraded
    } else {
        ShadowkeepPassState::DisabledAsAbsent
    };
    vec![
        ShadowkeepPassRecord {
            name: "opaque / G-buffer",
            state: ShadowkeepPassState::Ready,
            evidence: "Shadowkeep static, terrain, and rigid submissions produce non-empty depth and material targets",
        },
        ShadowkeepPassRecord {
            name: "global lighting",
            state: lighting_state,
            evidence: "legacy fullscreen technique binds and evaluates constants; a non-trivial light-target capture is still required",
        },
        ShadowkeepPassRecord {
            name: "deferred shading",
            state: deferred_state,
            evidence: "legacy deferred technique is loaded only when its bootstrap entry is present; output capture gate is pending",
        },
        ShadowkeepPassRecord {
            name: "transparent / decal / water / atmosphere",
            state: ShadowkeepPassState::DisabledAsAbsent,
            evidence: "not admitted until an era-specific producer and pass-order capture is available",
        },
    ]
}

impl ShadowkeepInputLayouts {
    pub fn load(gpu: &Gpu, input_layouts: &SShadowkeepVertexInputLayouts) -> anyhow::Result<Self> {
        let mut layouts = std::iter::repeat_with(|| None).take(255).collect::<Vec<_>>();
        for index in 0..7 {
            let d3d_layout = RenderStates::create_base_input_layout(&gpu.device, index)
                .with_context(|| format!("Failed to create Shadowkeep built-in input layout {index}"))?;
            d3d_layout.set_debug_name(format!("Shadowkeep Built-in Input Layout {index}"));
            layouts[index] = Some(d3d_layout);
        }
        tracing::warn!(
            mapping_layouts = ?input_layouts.mapping.layouts.iter().map(|layout| layout.index).collect::<Vec<_>>(),
            "Shadowkeep serialized input layout indices"
        );
        for layout in &input_layouts.mapping.layouts {
            let index = layout.index as usize;
            ensure!(index < layouts.len(), "Shadowkeep input layout index {index} exceeds 254");
            let mut elements = Vec::new();
            for (buffer_index, set_index) in [layout.element_0, layout.element_1, layout.element_2, layout.element_3]
                .into_iter()
                .enumerate()
            {
                if set_index == u32::MAX {
                    continue;
                }
                let set = input_layouts
                    .element_sets
                    .sets
                    .get(set_index as usize)
                    .with_context(|| format!("input layout {index} references missing element set {set_index}"))?;
                for element in &set.elements {
                    let semantic = INPUT_SEMANTICS
                        .get(element.semantic as usize)
                        .with_context(|| format!("input layout {index} has invalid semantic {}", element.semantic))?;
                    let format = INPUT_FORMATS
                        .get(element.format as usize)
                        .with_context(|| format!("input layout {index} has invalid format {}", element.format))?;
                    ensure!(format.hlsl_type != "", "input layout {index} uses unsupported format {}", element.format);
                    elements.push(TigerInputLayoutElement {
                        hlsl_type: format.hlsl_type,
                        format: format.format,
                        _stride: format.stride,
                        semantic_name: semantic,
                        semantic_index: element.semantic_index as u32,
                        buffer_index: buffer_index as u32,
                        is_instance_data: false,
                    });
                }
            }
            if index == 1 {
                let signature = elements
                    .iter()
                    .map(|element| {
                        format!(
                            "{}{} {:?} slot{}",
                            element.semantic_name,
                            element.semantic_index,
                            element.format,
                            element.buffer_index
                        )
                    })
                    .collect::<Vec<_>>();
                tracing::warn!(?signature, "Shadowkeep layout 1 signature diagnostic");
            }
            let d3d_layout = RenderStates::create_input_layout(&gpu.device, &elements)
                .with_context(|| format!("Failed to create Shadowkeep input layout {index}"))?;
            d3d_layout.set_debug_name(format!("Shadowkeep Dynamic Input Layout {index}"));
            layouts[index] = Some(d3d_layout);
        }
        tracing::warn!(
            available_layouts = layouts.iter().filter(|layout| layout.is_some()).count(),
            layout_0 = layouts[0].is_some(),
            layout_1 = layouts[1].is_some(),
            layout_6 = layouts[6].is_some(),
            "Shadowkeep input layout availability diagnostic"
        );
        Ok(Self { layouts })
    }

    pub fn get(&self, index: usize) -> anyhow::Result<&d3d11::InputLayout> {
        self.layouts
            .get(index)
            .and_then(Option::as_ref)
            .with_context(|| format!("Shadowkeep input layout {index} is unavailable"))
    }
}

pub struct ShadowkeepRendererBootstrap {
    pub profile: ShadowkeepEraProfile,
    pub bootstrap: ShadowkeepRenderBootstrap,
    pub input_layouts: ShadowkeepInputLayouts,
    pub techniques: ShadowkeepTechniqueRegistry,
}

impl ShadowkeepRendererBootstrap {
    /// Stage one of renderer construction: only era-correct bootstrap and
    /// renderer-local resources are touched.  No modern render-global tag is
    /// read here.
    pub fn load(gpu: Arc<Gpu>) -> anyhow::Result<Self> {
        let profile = ShadowkeepEraProfile;
        let bootstrap = profile.load_bootstrap()?;
        let raw_input_layouts = package_manager()
            .read_tag_struct::<SShadowkeepVertexInputLayouts>(bootstrap.input_layouts)
            .context("Failed to read Shadowkeep input-layout resource")?;
        let input_layouts = ShadowkeepInputLayouts::load(&gpu, &raw_input_layouts)?;
        let asset_manager = AssetManager::new_shadowkeep(&gpu);
        let techniques = ShadowkeepTechniqueRegistry::new(gpu, asset_manager, &bootstrap);
        Ok(Self { profile, bootstrap, input_layouts, techniques })
    }

    pub fn capability_ledger(&self) -> Vec<CapabilityRecord> {
        bootstrap_capability_ledger()
    }
}
