use std::{any::Any, sync::Arc};

use ahash::AHashMap;
use alkahest_core::job::{SCHEDULER, potassium::JobHandle};
use alkahest_data::shadowkeep::{
    SShadowkeepDynamicMaterialVariant, SShadowkeepDynamicModel, lod_category_from_legacy,
    primitive_type_from_legacy,
};
use alkahest_data::tfx::{
    RenderStage, ShaderStage, TfxScopeBits,
    common::AxisAlignedBBox,
    features::dynamic::{
        RenderStageSubscription, SDynamicMesh, SDynamicMeshMaterialVariants, SDynamicMeshPart,
        SDynamicModel,
    },
};
use anyhow::Context;
use glam::{Mat4, Vec4, Vec4Swizzles};
use itertools::{Itertools, multizip};
use tiger_parse::PackageManagerExt;
use tiger_pkg::{TagHash, package_manager};

use super::{FeatureRenderer, shared::ModelBuffers};
use crate::{
    Renderer,
    asset::{Handle, handle::is_technique_loaded},
    gpu::{cbuffer::ConstantBuffer, command_list::CommandList},
    renderer::{
        provenance::{
            record_shadowkeep_sky_object_draw, record_shadowkeep_sky_technique_dependency,
        },
        visibility::OpaqueView,
    },
    tfx::{packet::CompactTransform, sequencer_vm::ObjectChannel, technique::Technique},
    util::threading::CommandListSetId,
};

pub struct DynamicModel {
    mesh_buffers: Vec<ModelBuffers>,

    technique_map: Vec<SDynamicMeshMaterialVariants>,
    techniques: Vec<Handle<Technique>>,

    pub model: SDynamicModel,
    pub mesh_stages: Vec<RenderStageSubscription>,
    pub subscribed_stages: RenderStageSubscription,
    part_techniques: Vec<Vec<Handle<Technique>>>,

    // pub selected_mesh: usize,
    pub permutation: usize,
    permutation_count: usize,

    identifier_count: usize,
    pub identifier_mask: u128,

    pub hash: TagHash,

    pub cb: ConstantBuffer<RigidModelConstants>,
    cbuffer_slot: u32,
    pub channels: AHashMap<u32, ObjectChannel>,
    pub transform: Mat4,
    sky_owner: Option<(TagHash, TagHash)>,
}

impl DynamicModel {
    #[profiling::function]
    pub fn load(
        hash: TagHash,
        technique_map: Vec<SDynamicMeshMaterialVariants>,
        techniques: Vec<TagHash>,
    ) -> anyhow::Result<Box<Self>> {
        let model = package_manager().read_tag_struct::<SDynamicModel>(hash)?;

        let techniques = techniques
            .iter()
            .map(|&tag| Renderer::instance().asset_manager.load(tag))
            .collect_vec();

        let mesh_buffers = model
            .meshes
            .iter()
            .map(|m| {
                ModelBuffers::load(
                    m.vertex0_buffer,
                    m.vertex1_buffer,
                    m.color_buffer,
                    m.index_buffer,
                )
                .expect("Failed to load model buffers for dynamic model")
            })
            .collect_vec();

        let mesh_stages = model
            .meshes
            .iter()
            .map(|m| RenderStageSubscription::from_partrange_list(&m.part_range_per_render_stage))
            .collect_vec();

        let part_techniques = model
            .meshes
            .iter()
            .map(|m| {
                m.parts
                    .iter()
                    .map(|p| Renderer::instance().asset_manager.load(p.technique))
                    .collect_vec()
            })
            .collect_vec();

        let permutation_count = technique_map
            .iter()
            .filter(|m| m.unk8 == 0)
            .map(|m| m.technique_count as usize)
            .next()
            .unwrap_or(1);

        let identifier_count = model
            .meshes
            .iter()
            .map(|m| {
                m.parts
                    .iter()
                    .map(|p| p.external_identifier)
                    .max()
                    .unwrap_or(0)
            })
            .max()
            .unwrap_or(0) as usize
            + 1;

        Ok(Box::new(Self {
            permutation: permutation_count - 1,
            permutation_count,
            // selected_mesh: 0,
            identifier_count,
            identifier_mask: u128::MAX,
            mesh_buffers,
            technique_map,
            techniques,
            model,
            subscribed_stages: mesh_stages
                .iter()
                .fold(RenderStageSubscription::empty(), |acc, &x| acc | x),
            mesh_stages,
            part_techniques,
            hash,
            cb: ConstantBuffer::create(&Renderer::instance().gpu, None)
                .context("Failed to create constant buffer")?,
            cbuffer_slot: 1,
            channels: AHashMap::default(),
            transform: Mat4::IDENTITY,
            sky_owner: None,
        }))
    }

    /// Normalize the Shadowkeep dynamic-model layout.  The 24th (compute
    /// skinning) stage is explicitly kept empty because it does not exist in
    /// the Arrivals format.
    pub fn load_shadowkeep(
        hash: TagHash,
        technique_map: Vec<SShadowkeepDynamicMaterialVariant>,
        techniques: Vec<TagHash>,
    ) -> anyhow::Result<Box<Self>> {
        let legacy = package_manager().read_tag_struct::<SShadowkeepDynamicModel>(hash)?;
        let model = SDynamicModel {
            file_size: legacy.file_size,
            unk8: legacy.unk8,
            meshes: legacy
                .meshes
                .iter()
                .map(|mesh| {
                    let mut part_ranges = [0u16; RenderStage::COUNT + 1];
                    part_ranges[..mesh.part_range_per_render_stage.len()]
                        .copy_from_slice(&mesh.part_range_per_render_stage);
                    part_ranges[RenderStage::COUNT] = mesh.part_range_per_render_stage[23];
                    let mut layouts = [0u8; RenderStage::COUNT];
                    for (index, layout) in mesh.input_layout_per_render_stage.iter().enumerate() {
                        layouts[index] = u8::try_from(*layout).context(
                            "Shadowkeep dynamic input-layout index exceeds u8",
                        )?;
                    }
                    Ok(SDynamicMesh {
                        vertex0_buffer: mesh.vertex0_buffer,
                        vertex1_buffer: mesh.vertex1_buffer,
                        buffer2: mesh.buffer2,
                        buffer3: mesh.buffer3,
                        index_buffer: mesh.index_buffer,
                        // The preserved dynamic renderer binds only vertex0,
                        // vertex1, and index data; no later color stream is
                        // implied by the two legacy auxiliary buffers.
                        color_buffer: TagHash::NONE,
                        skinning_buffer: TagHash::NONE,
                        unk1c: mesh.unk14,
                        parts: mesh.parts.iter().map(|part| {
                            Ok(SDynamicMeshPart {
                                technique: part.technique,
                                variant_shader_index: part.variant_shader_index,
                                primitive_type: primitive_type_from_legacy(part.primitive_type).context(
                                    "Shadowkeep dynamic part has an unsupported primitive topology",
                                )?,
                                unk7: part.unk7,
                                index_start: part.index_start,
                                index_count: part.index_count,
                                unk10: part.unk10,
                                external_identifier: part.external_identifier,
                                unk16: part.unk16,
                                flags: 0,
                                gear_dye_change_color_index: 0,
                                lod_category: lod_category_from_legacy(part.lod_category).context(
                                    "Shadowkeep dynamic part has an unsupported LOD category",
                                )?,
                                unk1e: 0,
                                lod_run: 0,
                                unk20: part.unk1c,
                            })
                        }).collect::<anyhow::Result<_>>()?,
                        part_range_per_render_stage: part_ranges,
                        input_layout_per_render_stage: layouts,
                        _pad7a: [0; 3],
                    })
                })
                .collect::<anyhow::Result<_>>()?,
            unk20: legacy.unk20,
            model_scale: legacy.model_scale,
            model_offset: legacy.model_offset,
            texcoord_scale: legacy.texcoord_scale,
            texcoord_offset: legacy.texcoord_offset,
        };
        let technique_map = technique_map
            .into_iter()
            .map(|variant| SDynamicMeshMaterialVariants {
                technique_count: variant.technique_count,
                technique_start: variant.technique_start,
                unk8: variant.unk8,
            })
            .collect_vec();
        let loaded_techniques = techniques
            .iter()
            .map(|&tag| Renderer::instance().asset_manager.load(tag))
            .collect_vec();
        let mesh_buffers = model
            .meshes
            .iter()
            .map(|mesh| {
                ModelBuffers::load(
                    mesh.vertex0_buffer,
                    mesh.vertex1_buffer,
                    mesh.color_buffer,
                    mesh.index_buffer,
                )
                .context("Failed to load Shadowkeep rigid-model buffers")
            })
            .collect::<anyhow::Result<_>>()?;
        let mesh_stages = model
            .meshes
            .iter()
            .map(|mesh| {
                RenderStageSubscription::from_partrange_list(&mesh.part_range_per_render_stage)
            })
            .collect_vec();
        let part_techniques = model
            .meshes
            .iter()
            .map(|mesh| {
                mesh.parts
                    .iter()
                    .map(|part| Renderer::instance().asset_manager.load(part.technique))
                    .collect_vec()
            })
            .collect_vec();
        let permutation_count = technique_map
            .iter()
            .filter(|variant| variant.unk8 == 0)
            .map(|variant| variant.technique_count as usize)
            .next()
            .unwrap_or(1)
            .max(1);
        let identifier_count = model
            .meshes
            .iter()
            .flat_map(|mesh| mesh.parts.iter().map(|part| part.external_identifier))
            .max()
            .unwrap_or(0) as usize
            + 1;
        let subscribed_stages = mesh_stages
            .iter()
            .fold(RenderStageSubscription::empty(), |stages, stage| {
                stages | *stage
            });
        Ok(Box::new(Self {
            mesh_buffers,
            technique_map,
            techniques: loaded_techniques,
            model,
            mesh_stages,
            subscribed_stages,
            part_techniques,
            permutation: permutation_count - 1,
            permutation_count,
            identifier_count,
            identifier_mask: u128::MAX,
            hash,
            cb: ConstantBuffer::create(&Renderer::instance().gpu, None)
                .context("Failed to create Shadowkeep rigid-model constant buffer")?,
            cbuffer_slot: Renderer::instance()
                .globals
                .scopes
                .rigid_model
                .vertex_slot() as u32,
            channels: AHashMap::default(),
            transform: Mat4::IDENTITY,
            sky_owner: None,
        }))
    }

    pub fn set_sky_owner(&mut self, map: TagHash, collection: TagHash) {
        self.sky_owner = Some((map, collection));
    }

    pub fn mesh_count(&self) -> usize {
        self.model.meshes.len()
    }

    pub fn variant_count(&self) -> usize {
        self.permutation_count
    }

    pub fn identifier_count(&self) -> usize {
        self.identifier_count
    }

    fn get_permutation_technique(
        &self,
        index: u16,
        permutation_count: usize,
    ) -> Option<Handle<Technique>> {
        if index == u16::MAX {
            None
        } else {
            self.technique_map
                .get(index as usize)
                .as_ref()
                .map(|permutation_range| {
                    self.techniques[permutation_range.technique_start as usize
                        + (permutation_count % permutation_range.technique_count as usize)]
                        .clone()
                })
        }
    }

    // /// ⚠ Expects the `rigid_model` scope to be bound
    // pub fn draw(
    //     &self,
    //     renderer: &Renderer,
    //     render_stage: TfxRenderStage,
    //     identifier: u16,
    //     object_channels: Option<&ObjectChannels>,
    // ) -> anyhow::Result<()> {
    //     self.draw_wrapped(
    //         renderer,
    //         render_stage,
    //         identifier,
    //         object_channels,
    //         |_, renderer, _mesh, part| unsafe {
    //             renderer
    //                 .gpu
    //                 .lock_context()
    //                 .DrawIndexed(part.index_count, part.index_start, 0);
    //         },
    //     )
    // }

    pub fn draw_wrapped<F>(
        &self,
        cmd: &mut CommandList,
        stage: RenderStage,
        identifier_mask: u128,
        mut f: F,
    ) where
        F: FnMut(&Self, &mut CommandList, (usize, &SDynamicMesh), (usize, &SDynamicMeshPart)),
    {
        cmd.disable_smart_technique_binding();
        for (mesh_index, (mesh, subscribed_stages, mesh_buffers, mesh_techniques)) in multizip((
            self.model.meshes.iter(),
            self.mesh_stages.iter(),
            self.mesh_buffers.iter(),
            self.part_techniques.iter(),
        ))
        .enumerate()
        {
            if !subscribed_stages.is_subscribed(stage) {
                continue;
            }

            self.cb.bind(cmd, ShaderStage::Vertex, self.cbuffer_slot);
            self.cb.bind(cmd, ShaderStage::Pixel, self.cbuffer_slot);

            if Renderer::instance()
                .set_input_layout(cmd, mesh.get_input_layout_for_stage(stage) as usize)
                .is_err()
            {
                continue;
            }
            mesh_buffers.bind(cmd);
            for part_index in mesh.get_range_for_stage(stage) {
                let part = &mesh.parts[part_index];
                if identifier_mask & 1u128.unbounded_shl(part.external_identifier as u32) == 0 {
                    continue;
                }

                if !part.lod_category.is_highest_detail() {
                    continue;
                }

                let variant_material =
                    self.get_permutation_technique(part.variant_shader_index, self.permutation);

                let mut all_scopes = TfxScopeBits::empty();
                mesh_techniques[part_index].get_ref(|technique| {
                    technique
                        .bind_with_channels(cmd, Some(&self.channels))
                        .expect("Failed to bind technique");
                    if let Some((map, collection)) = self.sky_owner {
                        record_shadowkeep_sky_technique_dependency(
                            map,
                            collection,
                            self.hash,
                            technique,
                            &self.channels,
                        );
                    }
                    all_scopes |= technique.used_scopes;
                });

                if let Some(technique) = &variant_material {
                    technique.get_ref(|tech| {
                        tech.bind_with_channels(cmd, Some(&self.channels))
                            .expect("Failed to bind variant technique");
                        if let Some((map, collection)) = self.sky_owner {
                            record_shadowkeep_sky_technique_dependency(
                                map,
                                collection,
                                self.hash,
                                tech,
                                &self.channels,
                            );
                        }
                        all_scopes |= tech.used_scopes;
                    });
                }

                // No technique, no scopes, no draw
                if all_scopes.is_empty() {
                    continue;
                }

                if all_scopes.contains(TfxScopeBits::SKINNING) {
                    cmd.vertex_set_shader(&Renderer::instance().common.disable_skinning_vs);
                }

                cmd.set_input_topology(part.primitive_type);

                f(self, cmd, (mesh_index, mesh), (part_index, part));
            }
        }
    }
}

#[profiling::all_functions]
impl FeatureRenderer for DynamicModel {
    fn visibility_test(&mut self, _view_index: usize, view: &dyn OpaqueView) -> bool {
        // TODO(cohae): frustum culling is broken for some moving models (such as the vertex animated fan segments in Irkalla Complex)
        let bounds = AxisAlignedBBox::from_center_extents(
            self.model.model_offset.xyz(),
            self.model.model_scale.xyz() * 2.0,
        )
        .transformed(self.transform);

        view.is_visible(&bounds)
    }

    fn prepare(&mut self, renderer: &Renderer, _view_index: usize, extracted_data: &dyn Any) {
        let (obj_local_to_world, permutation) = extracted_data
            .downcast_ref::<(CompactTransform, usize)>()
            .expect("Invalid extracted data type")
            .clone();
        self.transform = obj_local_to_world.to_mat4();
        self.permutation = permutation;

        self.cb
            .write(
                &renderer.gpu.context(),
                &RigidModelConstants {
                    mesh_to_world: obj_local_to_world.to_mat4(),
                    position_scale: self.model.model_scale,
                    position_offset: self.model.model_offset,
                    texcoord0_scale_offset: Vec4::new(
                        self.model.texcoord_scale.x,
                        self.model.texcoord_scale.y,
                        self.model.texcoord_offset.x,
                        self.model.texcoord_offset.y,
                    ),
                    dynamic_sh_ao_values: Vec4::new(
                        0.0,
                        0.0,
                        0.0,
                        if self.sky_owner.is_some() { 1.0 } else { 0.8 },
                    ),
                },
            )
            .unwrap();
    }

    // #[profiling::function]
    fn submit(&self, cmd: &mut CommandList, _view_index: usize, stage: RenderStage) {
        profiling::scope!("DynamicModel::draw");

        self.draw_wrapped(
            cmd,
            stage,
            self.identifier_mask,
            |model, cmd, _, (_, part)| {
                if let Some((map, collection)) = model.sky_owner {
                    record_shadowkeep_sky_object_draw(stage, map, collection, model.hash);
                }
                cmd.draw_indexed(part.index_count, part.index_start, 0);
            },
        );
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
        let identifier_maswk = self.identifier_mask;
        let job = SCHEDULER.job_builder("rigid_model").spawn(move || {
            let self_ref = unsafe { &*(self_p as *const Self) };
            let cmd = pool.get_command_list(set);
            self_ref.draw_wrapped(
                cmd,
                stage,
                identifier_maswk,
                |model, cmd, (_mesh_index, _mesh), (_part_index, part)| {
                    if let Some((map, collection)) = model.sky_owner {
                        record_shadowkeep_sky_object_draw(stage, map, collection, model.hash);
                    }
                    cmd.draw_indexed(part.index_count, part.index_start, 0);
                },
            );
        });
        jobs.push(job);
    }

    fn subscribed_stages(&self) -> RenderStageSubscription {
        self.subscribed_stages
    }

    fn is_loaded(&self) -> bool {
        if self
            .part_techniques
            .iter()
            .any(|v| v.iter().any(|t| !is_technique_loaded(t)))
        {
            return false;
        }

        if self.techniques.iter().any(|t| !is_technique_loaded(t)) {
            return false;
        }

        true
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[repr(C)]
pub struct RigidModelConstants {
    pub mesh_to_world: Mat4,          // c0-c3
    pub position_scale: Vec4,         // c4
    pub position_offset: Vec4,        // c5
    pub texcoord0_scale_offset: Vec4, // c6
    pub dynamic_sh_ao_values: Vec4,   // c7
}
