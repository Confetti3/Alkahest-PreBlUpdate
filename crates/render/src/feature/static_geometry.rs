use std::{any::Any, f32, io::Write, ops::Deref, sync::Arc};

use ahash::HashMap;
use alkahest_core::job::{
    SCHEDULER,
    potassium::{JobHandle, Priority},
};
use alkahest_data::tfx::{
    RenderStage, ShaderStage,
    common::AxisAlignedBBox,
    features::{
        ao::SStaticAmbientOcclusion,
        dynamic::RenderStageSubscription,
        statics::{
            SStaticInstanceTransform, SStaticMesh, SStaticMeshData, SStaticMeshInstances,
            SStaticSpecialMesh,
        },
    },
};
use alkahest_data::{
    shadowkeep::{
        SShadowkeepStaticMesh, SShadowkeepStaticMeshInstances, lod_category_from_legacy,
        primitive_type_from_legacy, render_stage_from_legacy,
    },
    tag::Tag,
};
use anyhow::Context;
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Quat, Vec3, Vec4};
use itertools::Itertools;
use rayon::iter::{IntoParallelRefMutIterator, ParallelIterator};
use smallvec::SmallVec;
use tiger_parse::PackageManagerExt;
use tiger_pkg::{TagHash, package_manager};

use super::{FeatureRenderer, shared::ModelBuffers};
use crate::{
    Gpu, Renderer,
    asset::{Handle, handle::is_technique_loaded},
    gpu::{cbuffer::ConstantBuffer, command_list::CommandList},
    renderer::visibility::OpaqueView,
    tfx::{
        packet::{CompactTransform, VisibilityMask},
        technique::Technique,
    },
    util::threading::CommandListSetId,
};

struct SpecialMesh {
    mesh: SStaticSpecialMesh,
    buffers: ModelBuffers,
    technique: Handle<Technique>,
}

impl Deref for SpecialMesh {
    type Target = SStaticSpecialMesh;

    fn deref(&self) -> &Self::Target {
        &self.mesh
    }
}

#[repr(C)]
#[derive(Pod, Zeroable, Clone, Copy)]
pub struct InstanceTransformBlock {
    pub transform: [Vec4; 3],
    pub params: Vec4,
}

pub struct StaticMesh {
    pub model: SStaticMesh,
    pub materials: Vec<Handle<Technique>>,
    pub hash: TagHash,
    pub subscribed_stages: RenderStageSubscription,
    buffers: Vec<ModelBuffers>,
    special_meshes: Vec<SpecialMesh>,
}

impl StaticMesh {
    #[profiling::function]
    pub fn load(hash: TagHash) -> anyhow::Result<Self> {
        let model = package_manager().read_tag_struct::<SStaticMesh>(hash)?;
        let materials = model
            .techniques
            .iter()
            .map(|&tag| Renderer::instance().asset_manager.load::<Technique>(tag))
            .collect();

        let buffers = model
            .opaque_meshes
            .buffers
            .iter()
            .map(
                |&(index_buffer, vertex0_buffer, vertex1_buffer, color_buffer)| {
                    ModelBuffers::load(vertex0_buffer, vertex1_buffer, color_buffer, index_buffer)
                        .expect("Failed to load static model opaque mesh buffers")
                },
            )
            .collect();

        let mut subscribed_stages = model
            .opaque_meshes
            .mesh_groups
            .iter()
            .fold(RenderStageSubscription::empty(), |acc, group| {
                acc | group.render_stage
            });

        let special_meshes = model
            .special_meshes
            .iter()
            .map(|mesh| {
                subscribed_stages |= mesh.render_stage;
                SpecialMesh {
                    mesh: mesh.clone(),
                    buffers: ModelBuffers::load(
                        mesh.vertex0_buffer,
                        mesh.vertex1_buffer,
                        mesh.color_buffer,
                        mesh.index_buffer,
                    )
                    .expect("Failed to load special mesh buffers"),
                    technique: Renderer::instance().asset_manager.load(mesh.technique),
                }
            })
            .collect();

        Ok(Self {
            hash,
            model,
            materials,
            buffers,
            special_meshes,
            subscribed_stages,
        })
    }

    /// Load the Arrivals static-mesh layout into the feature renderer's
    /// normalized runtime form.  The source structs deliberately remain in
    /// `data::shadowkeep`; only the common draw description is shared.
    pub fn load_shadowkeep(hash: TagHash) -> anyhow::Result<Self> {
        let legacy = package_manager().read_tag_struct::<SShadowkeepStaticMesh>(hash)?;
        let opaque_meshes = SStaticMeshData {
            file_size: legacy.opaque_meshes.file_size,
            mesh_groups: legacy
                .opaque_meshes
                .mesh_groups
                .iter()
                .map(|group| {
                    Ok(alkahest_data::tfx::features::statics::SStaticMeshGroup {
                        part_index: group.part_index,
                        render_stage: render_stage_from_legacy(group.render_stage).context(
                            "Shadowkeep static group has a stage outside the 23-stage graph",
                        )?,
                        input_layout_index: group.input_layout_index,
                        unk5: group.unk5,
                        // The later resource narrowed this field.  It is a
                        // feature flag in the draw path, so retain its low
                        // byte instead of reinterpreting its meaning.
                        unk6: group.unk6 as u8,
                    })
                })
                .collect::<anyhow::Result<_>>()?,
            parts: legacy
                .opaque_meshes
                .parts
                .iter()
                .map(|part| {
                    Ok(alkahest_data::tfx::features::statics::SStaticMeshPart {
                        index_start: part.index_start,
                        index_count: part.index_count,
                        buffer_index: part.buffer_index,
                        unk9: part.unk9,
                        lod_category: lod_category_from_legacy(part.lod_category)
                            .context("Shadowkeep static part has an unsupported LOD category")?,
                        primitive_type: primitive_type_from_legacy(part.primitive_type).context(
                            "Shadowkeep static part has an unsupported primitive topology",
                        )?,
                    })
                })
                .collect::<anyhow::Result<_>>()?,
            buffers: legacy.opaque_meshes.buffers.clone(),
            unk38: 0,
            mesh_offset: legacy.mesh_offset,
            mesh_scale: legacy.mesh_scale,
            // Arrivals stores two scale components. The preserved renderer
            // consumed the first scalar in this shared static cbuffer.
            texture_coordinate_scale: legacy.texture_coordinate_scale.x,
            texture_coordinate_offset: legacy.texture_coordinate_offset,
            max_color_index: 0,
        };
        let special_meshes = legacy
            .special_meshes
            .iter()
            .map(|mesh| {
                Ok(alkahest_data::tfx::features::statics::SStaticSpecialMesh {
                    render_stage: render_stage_from_legacy(mesh.render_stage).context(
                        "Shadowkeep static special mesh has a stage outside the 23-stage graph",
                    )?,
                    input_layout_index: mesh.input_layout_index,
                    lod: lod_category_from_legacy(mesh.lod_category).context(
                        "Shadowkeep static special mesh has an unsupported LOD category",
                    )?,
                    unk3: mesh.unk5,
                    primitive_type: primitive_type_from_legacy(mesh.primitive_type).context(
                        "Shadowkeep static special mesh has an unsupported primitive topology",
                    )?,
                    unk5: mesh.unk7,
                    unk6: mesh.unk2,
                    index_buffer: mesh.index_buffer,
                    vertex0_buffer: mesh.vertex0_buffer,
                    vertex1_buffer: mesh.vertex1_buffer,
                    // This buffer did not exist in the Arrivals record.
                    color_buffer: TagHash::NONE,
                    index_start: mesh.index_start,
                    index_count: mesh.index_count,
                    technique: mesh.technique,
                })
            })
            .collect::<anyhow::Result<_>>()?;
        let model = SStaticMesh {
            file_size: legacy.file_size,
            opaque_meshes: Tag::with_hash(opaque_meshes, legacy.opaque_meshes.taghash()),
            unkc: legacy.unkc,
            techniques: legacy.techniques,
            special_meshes,
            unk30: legacy.unk30,
            unk38: legacy.unk38,
            unk50: [0; 4],
            unk60: [0; 4],
        };
        let materials = model
            .techniques
            .iter()
            .map(|&tag| Renderer::instance().asset_manager.load::<Technique>(tag))
            .collect();
        let buffers = model
            .opaque_meshes
            .buffers
            .iter()
            .map(|&(index, vertex0, vertex1, color)| {
                ModelBuffers::load(vertex0, vertex1, color, index)
                    .context("Failed to load Shadowkeep static model buffers")
            })
            .collect::<anyhow::Result<_>>()?;
        let mut subscribed_stages = model
            .opaque_meshes
            .mesh_groups
            .iter()
            .fold(RenderStageSubscription::empty(), |stages, group| {
                stages | group.render_stage
            });
        let special_meshes = model
            .special_meshes
            .iter()
            .map(|mesh| {
                subscribed_stages |= mesh.render_stage;
                Ok(SpecialMesh {
                    mesh: mesh.clone(),
                    buffers: ModelBuffers::load(
                        mesh.vertex0_buffer,
                        mesh.vertex1_buffer,
                        mesh.color_buffer,
                        mesh.index_buffer,
                    )
                    .context("Failed to load Shadowkeep static special-mesh buffers")?,
                    technique: Renderer::instance().asset_manager.load(mesh.technique),
                })
            })
            .collect::<anyhow::Result<_>>()?;
        Ok(Self {
            model,
            materials,
            hash,
            subscribed_stages,
            buffers,
            special_meshes,
        })
    }

    #[profiling::function]
    pub fn render_all(&self, cmd: &mut CommandList, stage: RenderStage, instance_count: u32) {
        if !self.subscribed_stages.is_subscribed(stage) {
            return;
        }

        let renderer = Renderer::instance();
        if let Some(ao_vb) = renderer.ao_buffer.read().as_ref().and_then(|h| h.get()) {
            cmd.vertex_set_shader_resources(1, std::slice::from_ref(&ao_vb.srv.as_ref()));
        }

        // self.instance_buffer
        //     .bind_cbuffer(cmd, ShaderStage::Vertex, 1);

        let is_opaque = matches!(
            stage,
            RenderStage::ShadowGenerate | RenderStage::DepthPrepass | RenderStage::GenerateGbuffer
        );

        if is_opaque {
            let opaque_meshes = &self.model.opaque_meshes;
            for (i, group, part) in opaque_meshes
                .mesh_groups
                .iter()
                .enumerate()
                .map(|(i, g)| (i, g, &opaque_meshes.parts[g.part_index as usize]))
                .filter(|(_, g, p)| g.render_stage == stage && p.lod_category.is_highest_detail())
            {
                let buffers = &self.buffers[part.buffer_index as usize];
                if buffers.bind(cmd).is_none() {
                    continue;
                }

                if let Some(technique) = &self.materials.get(i).and_then(Handle::get) {
                    technique.bind(cmd).expect("Failed to bind technique");
                } else {
                    continue;
                }

                if renderer
                    .set_input_layout(cmd, group.input_layout_index as usize)
                    .is_err()
                {
                    continue;
                }
                cmd.set_input_topology(part.primitive_type);

                cmd.draw_indexed_instanced(
                    part.index_count,
                    instance_count,
                    part.index_start,
                    0,
                    0,
                );
            }
        }

        if !is_opaque {
            for mesh in self
                .special_meshes
                .iter()
                .filter(|m| m.mesh.render_stage == stage && m.mesh.lod.is_highest_detail())
            {
                if mesh.buffers.bind(cmd).is_none() {
                    continue;
                }

                if let Some(technique) = &mesh.technique.get() {
                    technique.bind(cmd).expect("Failed to bind technique");
                } else {
                    continue;
                }
                if renderer
                    .set_input_layout(cmd, mesh.input_layout_index as usize)
                    .is_err()
                {
                    continue;
                }
                cmd.set_input_topology(mesh.primitive_type);

                cmd.draw_indexed_instanced(
                    mesh.index_count,
                    instance_count,
                    mesh.index_start,
                    0,
                    0,
                );
            }
        }
    }

    // #[profiling::function]
    /// The draw closure is called with (cmd, part.index_count, part.index_start)
    pub fn render_group<F>(
        &self,
        cmd: &mut CommandList,
        stage: RenderStage,
        group: usize,
        bind_technique: bool,
        draw: F,
    ) where
        F: Fn(&mut CommandList, u32, u32),
    {
        profiling::scope!(
            "render static model group",
            &format!("model={}, group={}", self.hash, group)
        );

        let i = group;
        let group = &self.model.opaque_meshes.mesh_groups[i];
        if group.render_stage != stage {
            return;
        }

        let part = &self.model.opaque_meshes.parts[group.part_index as usize];
        if !part.lod_category.is_highest_detail() {
            return;
        }

        {
            profiling::scope!(
                "bind buffers",
                &format!("buffer_index={}", part.buffer_index)
            );
            let buffers = &self.buffers[part.buffer_index as usize];
            if buffers.bind(cmd).is_none() {
                return;
            }
        }

        if let Some(technique) = &self.materials.get(i).and_then(Handle::get) {
            if bind_technique {
                technique.bind(cmd).expect("Failed to bind technique");
            }
        } else {
            return;
        }

        if Renderer::instance()
            .set_input_layout(cmd, group.input_layout_index as usize)
            .is_err()
        {
            return;
        }
        cmd.set_input_topology(part.primitive_type);

        // cmd.draw_indexed_instanced(part.index_count, instance_count, part.index_start, 0, 0);
        draw(cmd, part.index_count, part.index_start);
    }
}

struct StaticInstanceGroup {
    pub transforms: Vec<SStaticInstanceTransform>,
    pub static_index: u16,
    pub cbuffer: ConstantBuffer<u8>,
    pub bounds: Vec<AxisAlignedBBox>,
    pub group_bounds: AxisAlignedBBox,
    pub visible: VisibilityMask,
    pub num_instances: u32,
}

impl StaticInstanceGroup {
    #[profiling::function]
    pub fn update_constants(
        &self,
        ctx: &d3d11::DeviceContext,
        model: &StaticMesh,
        vao_identifier: u64,
        ao: Option<&SStaticAmbientOcclusion>,
        shadowkeep_layout: bool,
    ) {
        let mut data = vec![];
        let model = &model.model.opaque_meshes;
        let vao_base = ao.and_then(|ao| ao.get_offset_by_identifier(vao_identifier));

        if !shadowkeep_layout {
            data.write_all(bytemuck::cast_slice(&[
                model.mesh_offset.x,
                model.mesh_offset.y,
                model.mesh_offset.z,
                model.mesh_scale,
                model.texture_coordinate_scale,
                model.texture_coordinate_offset.x,
                model.texture_coordinate_offset.y,
                f32::from_bits(model.max_color_index),
            ]))
            .unwrap();
        }

        let model_transform = Mat4::from_cols(
            Vec4::new(model.mesh_scale, 0.0, 0.0, model.mesh_offset.x),
            Vec4::new(0.0, model.mesh_scale, 0.0, model.mesh_offset.y),
            Vec4::new(0.0, 0.0, model.mesh_scale, model.mesh_offset.z),
            Vec4::W,
        );
        for transform in self.transforms.iter() {
            let instance_transform = Mat4::from_scale_rotation_translation(
                Vec3::splat(transform.scale),
                transform.rotation,
                transform.translation,
            )
            .transpose();
            let instance_transform = if shadowkeep_layout {
                model_transform * instance_transform
            } else {
                instance_transform
            };

            let vertex_ao_offset = if let Some(vao_base) = vao_base {
                (transform.vertex_ao_offset + vao_base) >> 2
            } else {
                // println!("No AO for static model instance 0x{:016X}", self.identifier);
                0xFFFF_FFFF
            };

            let params = if shadowkeep_layout {
                Vec4::new(
                    model.texture_coordinate_scale,
                    model.texture_coordinate_offset.x,
                    model.texture_coordinate_offset.y,
                    f32::from_bits(model.max_color_index),
                )
            } else {
                Vec4::new(1.0, 1.0, 1.0, f32::from_bits(vertex_ao_offset))
            };
            data.write_all(bytemuck::cast_slice(&[InstanceTransformBlock {
                transform: [
                    instance_transform.x_axis,
                    instance_transform.y_axis,
                    instance_transform.z_axis,
                ],
                params,
            }]))
            .unwrap();
        }

        unsafe {
            self.cbuffer.write_array(ctx, &data).unwrap();
        }
    }
}

pub struct StaticInstancesRenderer {
    subscribed_stages: RenderStageSubscription,

    static_models: Vec<StaticMesh>,
    groups: Vec<StaticInstanceGroup>,

    constants_dirty: bool,
    shadowkeep_layout: bool,
    vao_identifier: u64,
    groups_by_stage_sorted_by_technique: HashMap<RenderStage, Arc<Vec<SortedModel>>>,
    bounds: AxisAlignedBBox,
}

impl StaticInstancesRenderer {
    /// World-space bounds reconstructed from the legacy instance placements.
    /// Shadowkeep collections do not carry the post-BL occlusion table, so
    /// callers use this conservative value for framing and diagnostics only.
    pub fn bounds(&self) -> AxisAlignedBBox {
        self.bounds
    }

    pub fn load(gpu: &Arc<Gpu>, instances_hash: TagHash) -> anyhow::Result<Self> {
        let instances: SStaticMeshInstances = package_manager().read_tag_struct(instances_hash)?;
        let mut static_models = Vec::with_capacity(instances.statics.len());

        for model in &instances.statics {
            let renderer = StaticMesh::load(*model)?;

            static_models.push(renderer);
        }

        let mut model_to_instance_groups: HashMap<u16, SmallVec<[usize; 4]>> = HashMap::default();
        let mut groups = Vec::with_capacity(instances.instance_groups.len());
        for (i, group) in instances.instance_groups.iter().enumerate() {
            let range = (group.instance_start as usize)
                ..(group.instance_start + group.instance_count) as usize;

            let transforms = instances
                .transforms
                .get(range.clone())
                .context("Invalid instance transform range")?
                .iter()
                .cloned()
                .collect_vec();

            let mut bounds = Vec::with_capacity(transforms.len());
            for i in range {
                let bounds_index = if instances.transform_to_bounds_index.is_empty() {
                    i
                } else {
                    *instances
                        .transform_to_bounds_index
                        .get(i)
                        .context("Invalid transform to bounds index")? as usize
                };
                let b = instances
                    .occlusion_bounds
                    .bounds
                    .get(bounds_index)
                    .context("Invalid occlusion bounds index")?;
                bounds.push(b.bb);
            }

            let group_bounds = bounds.iter().cloned().sum();

            let cbuffer = ConstantBuffer::create_raw_cb(
                gpu,
                2 * size_of::<Vec4>() // quantization headers
                            + transforms.len() * size_of::<InstanceTransformBlock>(), // per-transform data
            )?;

            groups.push(StaticInstanceGroup {
                num_instances: transforms.len() as u32,
                transforms,
                bounds,
                group_bounds,
                static_index: group.static_index,
                cbuffer,
                visible: VisibilityMask::default(),
            });
            model_to_instance_groups
                .entry(group.static_index)
                .or_default()
                .push(i);
        }

        let mut groups_by_stage_sorted_by_technique: HashMap<RenderStage, Vec<SortedModel>> =
            HashMap::default();
        for (model_index, model) in static_models.iter().enumerate() {
            for (group_index, (group, technique)) in model
                .model
                .opaque_meshes
                .mesh_groups
                .iter()
                .zip(model.materials.iter())
                .enumerate()
            {
                let part = &model.model.opaque_meshes.parts[group.part_index as usize];
                if part.lod_category.is_highest_detail() {
                    groups_by_stage_sorted_by_technique
                        .entry(group.render_stage)
                        .or_default()
                        .push(SortedModel {
                            technique: technique.hash(),
                            model_index,
                            group_index,
                            instance_groups: model_to_instance_groups
                                .get(&(model_index as u16))
                                .cloned()
                                .unwrap_or_default(),
                        });
                }
            }
        }

        for groups_sorted_by_technique in groups_by_stage_sorted_by_technique.values_mut() {
            groups_sorted_by_technique.sort_unstable_by_key(|k| k.technique);
        }

        let groups_by_stage_sorted_by_technique: HashMap<RenderStage, Arc<Vec<SortedModel>>> =
            groups_by_stage_sorted_by_technique
                .into_iter()
                .map(|(k, v)| (k, Arc::new(v)))
                .collect();

        Ok(Self {
            subscribed_stages: static_models
                .iter()
                .fold(RenderStageSubscription::empty(), |acc, m| {
                    acc | m.subscribed_stages
                }),
            static_models,
            groups,
            constants_dirty: true,
            shadowkeep_layout: false,
            vao_identifier: instances.vertex_ao_identifier,
            groups_by_stage_sorted_by_technique,
            bounds: instances.bounds,
        })
    }

    /// Resolve the legacy static-instance collection without routing it
    /// through a post-BL serialized type. Bounds are derived from placements
    /// because Arrivals collections do not carry the newer occlusion table.
    pub fn load_shadowkeep(gpu: &Arc<Gpu>, instances_hash: TagHash) -> anyhow::Result<Self> {
        let instances: SShadowkeepStaticMeshInstances =
            package_manager().read_tag_struct(instances_hash)?;
        let static_models = instances
            .statics
            .iter()
            .map(|&hash| StaticMesh::load_shadowkeep(hash))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut model_to_instance_groups: HashMap<u16, SmallVec<[usize; 4]>> = HashMap::default();
        let mut groups = Vec::with_capacity(instances.instance_groups.len());
        for (index, group) in instances.instance_groups.iter().enumerate() {
            let range = group.transform_range();
            let legacy_transforms = instances
                .transforms
                .get(range)
                .context("Invalid Shadowkeep static instance transform range")?;
            let transforms = legacy_transforms
                .iter()
                .map(|transform| SStaticInstanceTransform {
                    rotation: transform.rotation,
                    translation: transform.translation,
                    // Reference static submission uses the X component.
                    scale: transform.scale.x,
                    unk20: [transform.unk28, transform.unk2c],
                    vertex_ao_offset: 0,
                    unk2c: 0.0,
                    unk30: [0; 4],
                })
                .collect_vec();
            let bounds = transforms
                .iter()
                .map(|transform| {
                    AxisAlignedBBox::from_center_extents(
                        transform.translation,
                        Vec3::splat(transform.scale.abs()),
                    )
                })
                .collect_vec();
            let group_bounds = bounds.iter().copied().sum();
            let cbuffer = ConstantBuffer::create_raw_cb(
                gpu,
                transforms.len() * size_of::<InstanceTransformBlock>(),
            )?;
            groups.push(StaticInstanceGroup {
                num_instances: transforms.len() as u32,
                transforms,
                bounds,
                group_bounds,
                static_index: group.static_index,
                cbuffer,
                visible: VisibilityMask::default(),
            });
            model_to_instance_groups
                .entry(group.static_index)
                .or_default()
                .push(index);
        }
        let mut by_stage: HashMap<RenderStage, Vec<SortedModel>> = HashMap::default();
        for (model_index, model) in static_models.iter().enumerate() {
            for (group_index, (group, technique)) in model
                .model
                .opaque_meshes
                .mesh_groups
                .iter()
                .zip(&model.materials)
                .enumerate()
            {
                let part = &model.model.opaque_meshes.parts[group.part_index as usize];
                if part.lod_category.is_highest_detail() {
                    by_stage
                        .entry(group.render_stage)
                        .or_default()
                        .push(SortedModel {
                            technique: technique.hash(),
                            model_index,
                            group_index,
                            instance_groups: model_to_instance_groups
                                .get(&(model_index as u16))
                                .cloned()
                                .unwrap_or_default(),
                        });
                }
            }
        }
        for models in by_stage.values_mut() {
            models.sort_unstable_by_key(|model| model.technique);
        }
        let groups_by_stage_sorted_by_technique = by_stage
            .into_iter()
            .map(|(stage, models)| (stage, Arc::new(models)))
            .collect();
        let bounds = groups.iter().map(|group| group.group_bounds).sum();
        Ok(Self {
            subscribed_stages: static_models
                .iter()
                .fold(RenderStageSubscription::empty(), |stages, model| {
                    stages | model.subscribed_stages
                }),
            static_models,
            groups,
            constants_dirty: true,
            shadowkeep_layout: true,
            vao_identifier: 0,
            groups_by_stage_sorted_by_technique,
            bounds,
        })
    }
}

#[profiling::all_functions]
impl FeatureRenderer for StaticInstancesRenderer {
    fn visibility_test(&mut self, view_index: usize, view: &dyn OpaqueView) -> bool {
        if !view.is_visible(&self.bounds) {
            return false;
        }

        self.groups.par_iter_mut().for_each(|group| {
            group
                .visible
                .set(view_index, view.is_visible(&group.group_bounds));
            if group.visible.get(view_index) {
                group.visible.set(
                    view_index,
                    group.bounds.iter().any(|bb| view.is_visible(bb)),
                );
            }
        });

        self.groups.iter().any(|m| m.visible.get(view_index))
    }

    fn prepare(
        &mut self,
        renderer: &Renderer,
        _view_index: usize,
        _extracted_data: &dyn std::any::Any,
    ) {
        if self.constants_dirty {
            for group in &self.groups {
                let model = &self.static_models[group.static_index as usize];
                group.update_constants(
                    &renderer.gpu.context(),
                    model,
                    self.vao_identifier,
                    renderer.ao.read().as_ref(),
                    self.shadowkeep_layout,
                );
            }
            self.constants_dirty = false;
        }
    }

    fn submit(&self, cmd: &mut CommandList, view_index: usize, stage: RenderStage) {
        for group in self.groups.iter().filter(|g| {
            let model = &self.static_models[g.static_index as usize];

            g.visible.get(view_index) && model.subscribed_stages.is_subscribed(stage)
        }) {
            let model = &self.static_models[group.static_index as usize];
            group.cbuffer.bind_cbuffer(
                cmd,
                ShaderStage::Vertex,
                Renderer::instance()
                    .globals
                    .scopes
                    .chunk_model
                    .vertex_slot() as u32,
            );
            model.render_all(cmd, stage, group.num_instances);
        }
    }

    fn submit_parallel(
        &self,
        _renderer: &Arc<Renderer>,
        view_index: usize,
        set: CommandListSetId,
        stage: RenderStage,
        jobs: &mut Vec<JobHandle>,
    ) {
        let renderer = Renderer::instance();

        let Some(groups_sorted_by_technique) = self.groups_by_stage_sorted_by_technique.get(&stage)
        else {
            for (i, _group) in self.groups.iter().enumerate().filter(|(_i, g)| {
                let model = &self.static_models[g.static_index as usize];
                g.visible.get(view_index) && model.subscribed_stages.is_subscribed(stage)
            }) {
                let p_models = &self.static_models as *const _ as u64;
                let p_groups = &self.groups as *const _ as u64;
                let pool_clone = renderer.cmd_pool.clone();
                let job = SCHEDULER
                    .job_builder("static_geometry")
                    .priority(Priority::High)
                    .spawn(move || {
                        let cmd = pool_clone.get_command_list(set);
                        let renderer = Renderer::instance();
                        if let Some(ao_vb) =
                            renderer.ao_buffer.read().as_ref().and_then(|h| h.get())
                        {
                            cmd.vertex_set_shader_resources(
                                1,
                                std::slice::from_ref(&ao_vb.srv.as_ref()),
                            );
                        }

                        // Safety: p_models/p_groups are (practically) valid for the lifetime of this closure
                        // TODO(cohae): need a safer way to pass self.models to the job
                        let p_models = p_models as *const Vec<StaticMesh>;
                        let models = unsafe { &*p_models };
                        let p_groups = p_groups as *const Vec<StaticInstanceGroup>;
                        let groups = unsafe { &*p_groups };
                        let group = &groups[i];
                        let model = &models[group.static_index as usize];
                        group.cbuffer.bind_cbuffer(
                            cmd,
                            ShaderStage::Vertex,
                            renderer.globals.scopes.chunk_model.vertex_slot() as u32,
                        );
                        model.render_all(cmd, stage, group.num_instances);
                    });

                jobs.push(job);
            }
            return;
        };

        let node_count = groups_sorted_by_technique.len();
        // let nodes_per_job = node_count / job_count;
        // let mut last_end = 0;
        // let mut jobs_scheduled = 0;
        let mut schedule_range = |range: std::ops::Range<usize>| {
            let groups_sorted_by_technique = groups_sorted_by_technique.clone();
            let p_models = &self.static_models as *const _ as u64;
            let p_groups = &self.groups as *const _ as u64;
            let pool_clone = renderer.cmd_pool.clone();

            let visible = groups_sorted_by_technique[range.clone()].iter().any(|r| {
                let group_indices = &r.instance_groups;
                group_indices
                    .iter()
                    .any(|&gi| self.groups[gi].visible.get(view_index))
            });

            if !visible {
                return;
            }

            let job = SCHEDULER
                .job_builder("static_geometry")
                .priority(Priority::High)
                .spawn(move || {
                    let cmd = pool_clone.get_command_list(set);

                    let renderer = Renderer::instance();
                    if let Some(ao_vb) = renderer.ao_buffer.read().as_ref().and_then(|h| h.get()) {
                        cmd.vertex_set_shader_resources(
                            1,
                            std::slice::from_ref(&ao_vb.srv.as_ref()),
                        );
                    }

                    // Safety: p_models/p_groups are (practically) valid for the lifetime of this closure
                    // TODO(cohae): need a safer way to pass self.models to the job
                    let p_models = p_models as *const Vec<StaticMesh>;
                    let models = unsafe { &*p_models };
                    let p_groups = p_groups as *const Vec<StaticInstanceGroup>;
                    let groups = unsafe { &*p_groups };

                    let mut bind_technique = true;
                    for range in &groups_sorted_by_technique[range.clone()] {
                        let model = &models[range.model_index];

                        model.render_group(
                            cmd,
                            stage,
                            range.group_index,
                            bind_technique,
                            |cmd, index_count, index_start| {
                                for group_index in &range.instance_groups {
                                    let group = &groups[*group_index];
                                    group.cbuffer.bind_cbuffer(
                                        cmd,
                                        ShaderStage::Vertex,
                                        renderer.globals.scopes.chunk_model.vertex_slot() as u32,
                                    );
                                    if group.visible.get(view_index) {
                                        cmd.draw_indexed_instanced(
                                            index_count,
                                            group.num_instances,
                                            index_start,
                                            0,
                                            0,
                                        );
                                    }
                                }
                            },
                        );

                        bind_technique = false;
                    }
                });

            jobs.push(job);
        };

        let mut last_technique: Option<TagHash> = None;
        let mut last_range_start = 0;
        for (i, model_range) in groups_sorted_by_technique.iter().enumerate() {
            if last_technique.is_none() {
                last_technique = Some(groups_sorted_by_technique[i].technique);
            }

            if Some(model_range.technique) != last_technique {
                let range = last_range_start..i;
                schedule_range(range.clone());
                last_technique = Some(model_range.technique);
                last_range_start = i;
            }
        }

        if last_range_start < node_count {
            let range = last_range_start..node_count;
            schedule_range(range.clone());
        }

        // let Some(groups_sorted_by_technique) = self.groups_by_stage_sorted_by_technique.get(&stage)
        // else {
        //     for (model, _visible) in self.models.iter().filter(|(m, v)| {
        //         *v && m
        //             .model
        //             .special_meshes
        //             .iter()
        //             .any(|s| s.render_stage == stage)
        //     }) {
        //         let p_model = &raw const *model as u64;
        //         let pool_clone = renderer.cmd_pool.clone();
        //         let job = SCHEDULER
        //             .job_builder("static_geometry_special_meshes")
        //             .priority(Priority::High)
        //             .spawn(move || {
        //                 let model_ref = unsafe { &*(p_model as *const StaticModelRenderer) };
        //                 let cmd = pool_clone.get_command_list(set);
        //                 model_ref.render_all(cmd, stage);
        //             });

        //         jobs.push(job);
        //     }

        //     return;
        // };

        // let node_count = groups_sorted_by_technique.len();
        // // let nodes_per_job = node_count / job_count;
        // // let mut last_end = 0;
        // // let mut jobs_scheduled = 0;
        // let mut schedule_range = |range: std::ops::Range<usize>| {
        //     let groups_sorted_by_technique = groups_sorted_by_technique.clone();
        //     let p_models = &self.models as *const _ as u64;
        //     let pool_clone = renderer.cmd_pool.clone();
        //     let job = SCHEDULER
        //         .job_builder("static_geometry")
        //         .priority(Priority::High)
        //         .spawn(move || {
        //             let cmd = pool_clone.get_command_list(set);
        //             // Safety: p_models is valid for the lifetime of this closure
        //             // TODO(cohae): need a better way to pass self.models to the job
        //             let p_models = p_models as *const Vec<(StaticModelRenderer, bool)>;
        //             let models = unsafe { &*p_models };
        //             for (_technique_hash, model_index, group_index) in
        //                 &groups_sorted_by_technique[range.clone()]
        //             {
        //                 let (model, visible) = &models[*model_index];
        //                 if *visible {
        //                     model.render_group(cmd, stage, *group_index);
        //                 }
        //             }
        //         });

        //     jobs.push(job);
        // };

        // let mut last_technique: Option<TagHash> = None;
        // let mut last_range_start = 0;
        // for (i, (technique, _model_index, _group_index)) in
        //     groups_sorted_by_technique.iter().enumerate()
        // {
        //     if last_technique.is_none() {
        //         last_technique = Some(groups_sorted_by_technique[i].0);
        //     }

        //     if Some(*technique) != last_technique {
        //         let range = last_range_start..i;
        //         schedule_range(range.clone());
        //         last_technique = Some(*technique);
        //         last_range_start = i;
        //     }
        // }

        // if last_range_start < node_count {
        //     let range = last_range_start..node_count;
        //     schedule_range(range.clone());
        // }
    }

    fn subscribed_stages(&self) -> RenderStageSubscription {
        self.subscribed_stages
    }

    fn is_loaded(&self) -> bool {
        if self
            .static_models
            .iter()
            .any(|v| v.materials.iter().any(|t| !is_technique_loaded(t)))
        {
            return false;
        }

        if self.static_models.iter().any(|v| {
            v.special_meshes
                .iter()
                .any(|s| !is_technique_loaded(&s.technique))
        }) {
            return false;
        }

        true
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

struct SortedModel {
    technique: TagHash,
    model_index: usize,
    group_index: usize,
    instance_groups: SmallVec<[usize; 4]>,
}

pub struct StaticModelRenderer {
    model: StaticMesh,
    group: StaticInstanceGroup,
    pub bounds: AxisAlignedBBox,
}

impl StaticModelRenderer {
    pub fn new(gpu: &Gpu, model: StaticMesh) -> anyhow::Result<Self> {
        let cbuffer = ConstantBuffer::create_raw(
            gpu,
            2 * size_of::<Vec4>() // quantization headers
                        +  size_of::<InstanceTransformBlock>(), // per-transform data
        )?;

        let om = &model.model.opaque_meshes;
        let bounds =
            AxisAlignedBBox::from_center_extents(om.mesh_offset, Vec3::splat(om.mesh_scale));

        let group = StaticInstanceGroup {
            transforms: vec![SStaticInstanceTransform {
                rotation: Quat::IDENTITY,
                translation: Vec3::ZERO,
                scale: 1.0,
                unk20: [0; 2],
                vertex_ao_offset: 0,
                unk2c: 0.0,
                unk30: [0; 4],
            }],
            static_index: 0,
            cbuffer,
            bounds: vec![],
            group_bounds: AxisAlignedBBox::NONE,
            visible: VisibilityMask::default(),
            num_instances: 1,
        };

        Ok(Self {
            model,
            group,
            bounds,
        })
    }
}

impl FeatureRenderer for StaticModelRenderer {
    fn prepare(&mut self, renderer: &Renderer, _view_index: usize, extracted_data: &dyn Any) {
        let (obj_local_to_world, _permutation) = extracted_data
            .downcast_ref::<(CompactTransform, usize)>()
            .expect("Invalid extracted data type")
            .clone();
        let transform = obj_local_to_world.to_mat4();

        let (scale, rotation, translation) = transform.to_scale_rotation_translation();
        let transform = &mut self.group.transforms[0];
        transform.rotation = rotation;
        transform.translation = translation;
        transform.scale = scale.x;

        self.group
            .update_constants(&renderer.gpu.context(), &self.model, 0, None, false);
    }

    fn submit(&self, cmd: &mut CommandList, _view_index: usize, stage: RenderStage) {
        self.group.cbuffer.bind_cbuffer(
            cmd,
            ShaderStage::Vertex,
            Renderer::instance()
                .globals
                .scopes
                .chunk_model
                .vertex_slot() as u32,
        );
        self.model.render_all(cmd, stage, 1);
    }

    fn submit_parallel(
        &self,
        renderer: &Arc<Renderer>,
        _view_index: usize,
        set: CommandListSetId,
        stage: RenderStage,
        jobs: &mut Vec<JobHandle>,
    ) {
        let self_p = &raw const *self as u64;
        let pool = renderer.cmd_pool.clone();
        let renderer = renderer.clone();
        let job = SCHEDULER.job_builder("rigid_model").spawn(move || {
            let self_ref = unsafe { &*(self_p as *const Self) };
            let cmd = pool.get_command_list(set);
            self_ref.group.cbuffer.bind_cbuffer(
                cmd,
                ShaderStage::Vertex,
                renderer.globals.scopes.chunk_model.vertex_slot() as u32,
            );
            self_ref.model.render_all(cmd, stage, 1);
        });
        jobs.push(job);
    }

    fn subscribed_stages(&self) -> RenderStageSubscription {
        self.model.subscribed_stages
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
