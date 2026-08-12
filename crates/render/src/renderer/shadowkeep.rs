//! Shadowkeep renderer resources that are intentionally owned by the renderer
//! rather than the package-independent D3D platform.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

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

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityId {
    Bootstrap,
    Tfx,
    CoreGeometry,
    LocalLighting,
    Presentation,
    CubemapIbl,
    GlobalLighting,
    Atmosphere,
    SkyObjects,
    SkyEnvironment,
    LightShaftOcclusion,
    MaterialAo,
    Solar,
    Transparent,
    DecalWaterVolumetrics,
    ActivityLayers,
}

impl CapabilityId {
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Bootstrap => "Shadowkeep bootstrap and input layouts",
            Self::Tfx => "TFX scopes, techniques, and positional channels",
            Self::CoreGeometry => "Core geometry submission",
            Self::LocalLighting => "Local lighting and deferred shading",
            Self::Presentation => "Direct presentation",
            Self::CubemapIbl => "Cubemap specular IBL",
            Self::GlobalLighting => "Global directional lighting and cascaded sun shadows",
            Self::Atmosphere => "Package atmosphere lookup/background",
            Self::SkyObjects => "Map-authored sky-object models",
            Self::SkyEnvironment => "Phase-specific sky-object environment selection",
            Self::LightShaftOcclusion => "Sky-object LightShaftOcclusion",
            Self::MaterialAo => "Shadowkeep material AO compatibility finalizer",
            Self::Solar => "Shadowkeep solar path",
            Self::Transparent => "General transparent/additive geometry",
            Self::DecalWaterVolumetrics => "Authored decals, water, and volumetrics",
            Self::ActivityLayers => "Activity and ambient scene layers",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilitySupport {
    Supported,
    Degraded { reason: String },
    Unsupported { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    MissingAsset,
    UnsupportedTechnique,
    InvalidInput,
    NotVisible,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PassDecision {
    Scheduled,
    Skipped(SkipReason),
    Blocked(String),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OutputMetrics {
    pub depth_coverage: f32,
    pub albedo_variance: f32,
    pub material_entropy: f32,
    pub final_luminance_p95: f32,
    pub non_finite_count: u64,
}

impl OutputMetrics {
    pub fn passes_invariants(&self) -> bool {
        self.non_finite_count == 0
            && self.depth_coverage.is_finite()
            && self.albedo_variance.is_finite()
            && self.material_entropy.is_finite()
            && self.final_luminance_p95.is_finite()
            && self.depth_coverage >= 0.001
            && self.albedo_variance >= 0.0001
            && self.material_entropy >= 0.01
            && self.final_luminance_p95 >= 0.001
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PassObservation {
    pub capability: CapabilityId,
    pub decision: PassDecision,
    pub draws_requested: u32,
    pub draws_submitted: u32,
    pub draws_skipped: BTreeMap<SkipReason, u32>,
    pub gpu_completed: bool,
    pub output_metrics: Option<OutputMetrics>,
    pub evidence_frame: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GraphicsReport {
    pub schema_version: u32,
    pub build: String,
    pub corpus_fingerprint: String,
    pub map: String,
    pub observations: Vec<PassObservation>,
}

impl GraphicsReport {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn new(
        build: String,
        corpus_fingerprint: String,
        map: String,
        observations: Vec<PassObservation>,
    ) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            build,
            corpus_fingerprint,
            map,
            observations,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityState {
    Ready,
    Degraded,
    Unavailable,
    Failed,
    NotExercised,
}

impl PassObservation {
    pub fn state(&self, support: &CapabilitySupport) -> CapabilityState {
        match support {
            CapabilitySupport::Unsupported { .. } => CapabilityState::Unavailable,
            CapabilitySupport::Degraded { .. } => CapabilityState::Degraded,
            CapabilitySupport::Supported => match self.decision {
                PassDecision::Skipped(_) => CapabilityState::NotExercised,
                PassDecision::Blocked(_) => CapabilityState::Failed,
                PassDecision::Scheduled
                    if self.draws_requested == 0 && self.draws_submitted == 0 =>
                {
                    CapabilityState::NotExercised
                }
                PassDecision::Scheduled
                    if self.draws_submitted == 0
                        || !self.gpu_completed
                        || !self
                            .output_metrics
                            .as_ref()
                            .is_some_and(OutputMetrics::passes_invariants) =>
                {
                    CapabilityState::Failed
                }
                PassDecision::Scheduled => CapabilityState::Ready,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct CapabilityRecord {
    pub id: CapabilityId,
    pub support: CapabilitySupport,
    pub evidence: String,
}

pub fn bootstrap_capability_ledger() -> Vec<CapabilityRecord> {
    use CapabilityId::*;
    let supported = [
        Bootstrap,
        Tfx,
        CoreGeometry,
        LocalLighting,
        Presentation,
        CubemapIbl,
        GlobalLighting,
        Atmosphere,
        SkyObjects,
        MaterialAo,
    ];
    let mut records = supported
        .into_iter()
        .map(|id| CapabilityRecord {
            id,
            support: CapabilitySupport::Supported,
            evidence: "Supported by the loaded renderer; runtime evidence not yet observed".into(),
        })
        .collect::<Vec<_>>();
    records.extend([
        CapabilityRecord {
            id: SkyEnvironment,
            support: CapabilitySupport::Degraded {
                reason: "activity-owned overlays are not proven for every active phase".into(),
            },
            evidence: "Exact collection and package-name selection is deterministic".into(),
        },
        CapabilityRecord {
            id: LightShaftOcclusion,
            support: CapabilitySupport::Unsupported {
                reason: "legacy light-shaft occlusion target/pass is not restored".into(),
            },
            evidence: "Authored 0x200 subscriptions are reported but not submitted".into(),
        },
        CapabilityRecord {
            id: Solar,
            support: CapabilitySupport::Degraded {
                reason: "admitted maps have no decoded authored solar track".into(),
            },
            evidence: "A scene-clock fallback drives sun direction and daylight".into(),
        },
        CapabilityRecord {
            id: Transparent,
            support: CapabilitySupport::Unsupported {
                reason: "general transparent/additive producers remain disconnected".into(),
            },
            evidence: "Only the isolated legacy SkyTransparent path is admitted".into(),
        },
        CapabilityRecord {
            id: DecalWaterVolumetrics,
            support: CapabilitySupport::Unsupported {
                reason: "era-specific producers and pass ordering are not connected".into(),
            },
            evidence: "No runtime submission path is scheduled".into(),
        },
        CapabilityRecord {
            id: ActivityLayers,
            support: CapabilitySupport::Degraded {
                reason: "no compatible package global-channel producer was found".into(),
            },
            evidence: "Base bubble and discovered freeroam scenario tables are admitted".into(),
        },
    ]);
    records
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
    pass_status_ledger_with_global_lighting(pipelines.global_lighting.is_available())
}

fn pass_status_ledger_with_global_lighting(
    global_lighting_available: bool,
) -> Vec<ShadowkeepPassRecord> {
    let global_lighting_state = if global_lighting_available {
        ShadowkeepPassState::Ready
    } else {
        ShadowkeepPassState::DisabledAsAbsent
    };
    vec![
        ShadowkeepPassRecord {
            name: "opaque / G-buffer",
            state: ShadowkeepPassState::Ready,
            evidence: "Shadowkeep static, terrain, and rigid submissions produce non-empty depth \
                       and material targets",
        },
        ShadowkeepPassRecord {
            name: "local diffuse / specular lighting",
            state: ShadowkeepPassState::Ready,
            evidence: "local-light volume draws produce non-zero diffuse and specular MRT captures",
        },
        ShadowkeepPassRecord {
            name: "cubemap specular IBL",
            state: ShadowkeepPassState::Ready,
            evidence: "authored cubemap volumes populate light_specular_ibl before deferred \
                       shading",
        },
        ShadowkeepPassRecord {
            name: "deferred shading",
            state: ShadowkeepPassState::Ready,
            evidence: "deferred_shading_no_atm consumes local and cubemap lighting into \
                       shading_result",
        },
        ShadowkeepPassRecord {
            name: "global directional lighting",
            state: global_lighting_state,
            evidence: "the legacy global-lighting pass is connected and enabled by default for \
                       Shadowkeep",
        },
        ShadowkeepPassRecord {
            name: "cascaded directional shadows",
            state: ShadowkeepPassState::Ready,
            evidence: "sun cascades produce a screen-space mask consumed through ShadowMask.unk00",
        },
        ShadowkeepPassRecord {
            name: "authentic atmosphere lookup and sky",
            state: ShadowkeepPassState::Ready,
            evidence: "package-authored atmosphere inputs populate both lookup targets; the \
                       preserved atmosphere-aware deferred and sky techniques produce finite \
                       output",
        },
        ShadowkeepPassRecord {
            name: "procedural solar fallback",
            state: ShadowkeepPassState::Degraded,
            evidence: "the synthetic scene-clock solar path remains explicit because the admitted \
                       target maps contain no decodable authored sun-angle track",
        },
        ShadowkeepPassRecord {
            name: "direct presentation",
            state: ShadowkeepPassState::Ready,
            evidence: "the authentic sky is composited only into clear-depth pixels before \
                       shading_result is copied to the sRGB output target",
        },
        ShadowkeepPassRecord {
            name: "transparent / decal / water / volumetrics",
            state: ShadowkeepPassState::DisabledAsAbsent,
            evidence: "not admitted until an era-specific producer and pass-order capture is \
                       available",
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        CapabilityId, CapabilityState, CapabilitySupport, OutputMetrics, PassDecision,
        PassObservation, ShadowkeepPassState, bootstrap_capability_ledger,
        pass_status_ledger_with_global_lighting,
    };

    #[test]
    fn transparent_capability_matches_disabled_runtime_pass() {
        let capability = bootstrap_capability_ledger()
            .into_iter()
            .find(|record| record.id == CapabilityId::Transparent)
            .expect("transparent capability must exist in the typed registry");
        let runtime_pass = pass_status_ledger_with_global_lighting(true)
            .into_iter()
            .find(|record| record.name == "transparent / decal / water / volumetrics")
            .expect("transparent pass must be represented in the runtime ledger");

        assert!(matches!(
            capability.support,
            CapabilitySupport::Unsupported { .. }
        ));
        assert_eq!(runtime_pass.state, ShadowkeepPassState::DisabledAsAbsent);
    }

    fn observation(metrics: OutputMetrics) -> PassObservation {
        PassObservation {
            capability: CapabilityId::CoreGeometry,
            decision: PassDecision::Scheduled,
            draws_requested: 10,
            draws_submitted: 10,
            draws_skipped: BTreeMap::new(),
            gpu_completed: true,
            output_metrics: Some(metrics),
            evidence_frame: 42,
        }
    }

    #[test]
    fn flat_or_black_output_cannot_be_ready() {
        let flat = observation(OutputMetrics {
            depth_coverage: 0.5,
            albedo_variance: 0.0,
            material_entropy: 0.0,
            final_luminance_p95: 0.5,
            non_finite_count: 0,
        });
        let black = observation(OutputMetrics {
            depth_coverage: 0.5,
            albedo_variance: 0.2,
            material_entropy: 0.5,
            final_luminance_p95: 0.0,
            non_finite_count: 0,
        });
        assert_eq!(
            flat.state(&CapabilitySupport::Supported),
            CapabilityState::Failed
        );
        assert_eq!(
            black.state(&CapabilitySupport::Supported),
            CapabilityState::Failed
        );
    }

    #[test]
    fn requested_draws_require_submission_and_completion() {
        let mut pass = observation(OutputMetrics {
            depth_coverage: 0.5,
            albedo_variance: 0.2,
            material_entropy: 0.5,
            final_luminance_p95: 0.5,
            non_finite_count: 0,
        });
        pass.draws_submitted = 0;
        assert_eq!(
            pass.state(&CapabilitySupport::Supported),
            CapabilityState::Failed
        );
        pass.draws_submitted = 10;
        pass.gpu_completed = false;
        assert_eq!(
            pass.state(&CapabilitySupport::Supported),
            CapabilityState::Failed
        );
    }

    #[test]
    fn graphics_report_serializes_versioned_typed_ids() {
        let report = super::GraphicsReport::new(
            "build".into(),
            "corpus".into(),
            "map".into(),
            vec![observation(OutputMetrics {
                depth_coverage: 0.5,
                albedo_variance: 0.2,
                material_entropy: 0.5,
                final_luminance_p95: 0.5,
                non_finite_count: 0,
            })],
        );
        let json = serde_json::to_value(report).unwrap();
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["observations"][0]["capability"], "core_geometry");
    }
}
