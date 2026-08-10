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
            state: CapabilityState::Ready,
            evidence: "static placements, terrain patches, and map-contained rigid models produce the shared G-buffer".into(),
        },
        CapabilityRecord {
            name: "Local lighting and deferred shading",
            state: CapabilityState::Ready,
            evidence: "local diffuse/specular MRTs and deferred_shading_no_atm produce non-trivial captures".into(),
        },
        CapabilityRecord {
            name: "Direct presentation",
            state: CapabilityState::Ready,
            evidence: "shading_result is presented directly through the sRGB output target".into(),
        },
        CapabilityRecord {
            name: "Cubemap specular IBL",
            state: CapabilityState::Ready,
            evidence: "authored cubemap volumes populate light_specular_ibl before deferred shading".into(),
        },
        CapabilityRecord {
            name: "Global directional lighting and cascaded sun shadows",
            state: CapabilityState::Ready,
            evidence: "the legacy global-lighting pass runs by default and consumes the screen-space cascade mask".into(),
        },
        CapabilityRecord {
            name: "Authentic atmosphere lookup and sky",
            state: CapabilityState::Ready,
            evidence: "authored map atmosphere placements feed preserved lookup-generation, atmosphere-aware deferred-shading, and sky techniques".into(),
        },
        CapabilityRecord {
            name: "Shadowkeep material AO and solar fallback",
            state: CapabilityState::Ready,
            evidence: "the era-specific finalizer preserves Arrivals material channels, normalizes RT2 material occlusion, and the scene clock drives a continuous sun/sky cycle".into(),
        },
        CapabilityRecord {
            name: "Transparent stages, decals, water, and volumetrics",
            state: CapabilityState::Blocked,
            evidence: "era-specific producers and pass ordering have not been connected".into(),
        },
        CapabilityRecord {
            name: "Activity and ambient scene layers",
            state: CapabilityState::Degraded,
            evidence: "base bubble and discovered freeroam scenario tables are admitted and unknown classes are counted; no compatible package global-channel producer was found in the admitted graph".into(),
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
    pub fn new(
        gpu: Arc<Gpu>,
        asset_manager: AssetManager,
        bootstrap: &ShadowkeepRenderBootstrap,
    ) -> Self {
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
        ensure!(
            tag.is_some(),
            "Shadowkeep technique {name} is explicitly null"
        );
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

/// Runtime pass ledger for the admitted Shadowkeep path. States describe the
/// connected graph, not merely whether a bootstrap technique can be loaded.
pub fn pass_status_ledger(pipelines: &GlobalPipelines) -> Vec<ShadowkeepPassRecord> {
    let global_lighting_state = if pipelines.global_lighting.is_available() {
        ShadowkeepPassState::Ready
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
            name: "local diffuse / specular lighting",
            state: ShadowkeepPassState::Ready,
            evidence: "local-light volume draws produce non-zero diffuse and specular MRT captures",
        },
        ShadowkeepPassRecord {
            name: "cubemap specular IBL",
            state: ShadowkeepPassState::Ready,
            evidence: "authored cubemap volumes populate light_specular_ibl before deferred shading",
        },
        ShadowkeepPassRecord {
            name: "deferred shading",
            state: ShadowkeepPassState::Ready,
            evidence: "deferred_shading_no_atm consumes local and cubemap lighting into shading_result",
        },
        ShadowkeepPassRecord {
            name: "global directional lighting",
            state: global_lighting_state,
            evidence: "the legacy global-lighting pass is connected and enabled by default for Shadowkeep",
        },
        ShadowkeepPassRecord {
            name: "cascaded directional shadows",
            state: ShadowkeepPassState::Ready,
            evidence: "sun cascades produce a screen-space mask consumed through ShadowMask.unk00",
        },
        ShadowkeepPassRecord {
            name: "authentic atmosphere lookup and sky",
            state: ShadowkeepPassState::Ready,
            evidence: "package-authored atmosphere inputs populate both lookup targets; the preserved atmosphere-aware deferred and sky techniques produce finite output",
        },
        ShadowkeepPassRecord {
            name: "procedural sky / sun fallback and material AO",
            state: ShadowkeepPassState::Degraded,
            evidence: "the fallback remains active while authored atmosphere inputs load; era-correct material AO remains incomplete",
        },
        ShadowkeepPassRecord {
            name: "direct presentation",
            state: ShadowkeepPassState::Ready,
            evidence: "the authentic sky is composited only into clear-depth pixels before shading_result is copied to the sRGB output target",
        },
        ShadowkeepPassRecord {
            name: "transparent / decal / water / volumetrics",
            state: ShadowkeepPassState::DisabledAsAbsent,
            evidence: "not admitted until an era-specific producer and pass-order capture is available",
        },
    ]
}

impl ShadowkeepInputLayouts {
    pub fn load(gpu: &Gpu, input_layouts: &SShadowkeepVertexInputLayouts) -> anyhow::Result<Self> {
        let mut layouts = std::iter::repeat_with(|| None)
            .take(255)
            .collect::<Vec<_>>();
        for index in 0..7 {
            let d3d_layout = RenderStates::create_base_input_layout(&gpu.device, index)
                .with_context(|| {
                    format!("Failed to create Shadowkeep built-in input layout {index}")
                })?;
            d3d_layout.set_debug_name(format!("Shadowkeep Built-in Input Layout {index}"));
            layouts[index] = Some(d3d_layout);
        }
        tracing::warn!(
            mapping_layouts = ?input_layouts.mapping.layouts.iter().map(|layout| layout.index).collect::<Vec<_>>(),
            "Shadowkeep serialized input layout indices"
        );
        for layout in &input_layouts.mapping.layouts {
            let index = layout.index as usize;
            ensure!(
                index < layouts.len(),
                "Shadowkeep input layout index {index} exceeds 254"
            );
            let mut elements = Vec::new();
            for (buffer_index, set_index) in [
                layout.element_0,
                layout.element_1,
                layout.element_2,
                layout.element_3,
            ]
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
                    .with_context(|| {
                        format!("input layout {index} references missing element set {set_index}")
                    })?;
                for element in &set.elements {
                    let semantic = INPUT_SEMANTICS
                        .get(element.semantic as usize)
                        .with_context(|| {
                            format!(
                                "input layout {index} has invalid semantic {}",
                                element.semantic
                            )
                        })?;
                    let format = INPUT_FORMATS
                        .get(element.format as usize)
                        .with_context(|| {
                            format!("input layout {index} has invalid format {}", element.format)
                        })?;
                    ensure!(
                        format.hlsl_type != "",
                        "input layout {index} uses unsupported format {}",
                        element.format
                    );
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
        Ok(Self {
            profile,
            bootstrap,
            input_layouts,
            techniques,
        })
    }

    pub fn capability_ledger(&self) -> Vec<CapabilityRecord> {
        bootstrap_capability_ledger()
    }
}
