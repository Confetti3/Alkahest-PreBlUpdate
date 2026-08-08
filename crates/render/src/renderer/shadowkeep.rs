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
            state: CapabilityState::Blocked,
            evidence: "map, static, terrain, dynamic, and rigid resource families are not yet normalized into the modern submitter".into(),
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

impl ShadowkeepInputLayouts {
    pub fn load(gpu: &Gpu, input_layouts: &SShadowkeepVertexInputLayouts) -> anyhow::Result<Self> {
        let mut layouts = std::iter::repeat_with(|| None).take(255).collect::<Vec<_>>();
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
            let d3d_layout = RenderStates::create_input_layout(&gpu.device, &elements)
                .with_context(|| format!("Failed to create Shadowkeep input layout {index}"))?;
            d3d_layout.set_debug_name(format!("Shadowkeep Dynamic Input Layout {index}"));
            layouts[index] = Some(d3d_layout);
        }
        Ok(Self { layouts })
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
        let asset_manager = AssetManager::new(&gpu);
        let techniques = ShadowkeepTechniqueRegistry::new(gpu, asset_manager, &bootstrap);
        Ok(Self { profile, bootstrap, input_layouts, techniques })
    }

    pub fn capability_ledger(&self) -> Vec<CapabilityRecord> {
        bootstrap_capability_ledger()
    }
}
