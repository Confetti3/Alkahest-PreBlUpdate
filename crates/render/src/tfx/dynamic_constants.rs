use std::sync::Arc;

use ahash::AHashMap;
use alkahest_data::tfx::{ExternIndex, SDynamicConstants, ShaderStage, shadowkeep::SShadowkeepDynamicConstants};
use anyhow::Context;
use glam::Vec4;
use itertools::Itertools;
use tiger_pkg::package_manager;

use super::expression_vm::{self, interpreter::InterpreterState};
use crate::{
    Gpu,
    asset::{
        AssetManager,
        Handle,
        texture::{Texture, load_sampler},
    },
    gpu::{
        cbuffer::ConstantBuffer,
        command_list::{CommandList, ContextExt},
    },
    tfx::{
        expression_vm::opcodes::{Opcode, OpcodeIterator},
        sequencer_vm::ObjectChannel,
    },
};

/// Holds all dynamically bound resources for a shader
pub struct DynamicConstants {
    pub textures: Vec<(u32, Option<Handle<Texture>>)>,
    /// The preserved pre-BL renderer kept a checker/white fallback bound for
    /// an unresolved material assignment.  Keep an era-neutral white SRV at
    /// the binding seam so a pending or failed optional texture does not
    /// leave the shader sampling an unbound slot (which presents as a black
    /// hole in the G-buffer).  The asset diagnostic still records the
    /// underlying queued/failed tag; this is only the draw-time fallback.
    pub fallback_texture: d3d11::ShaderResourceView,
    pub samplers: Vec<Option<d3d11::SamplerState>>,
    pub cbuffer_slot: u32,
    pub cbuffer: Option<ConstantBuffer<Vec4>>,
    pub bytecode: Vec<u8>,
    pub bytecode_constants: Vec<Vec4>,

    pub initial_constants: Vec<Vec4>,

    /// Indicates if the expression bytecode writes to the constant buffer. If this is false, then the cbuffer is not mapped for writing.
    pub writes_cbuffer: bool,
}

impl DynamicConstants {
    /// Loads resources through the renderer-local asset manager.  This is
    /// intentionally independent from the global renderer singleton so
    /// bootstrap techniques can be constructed before publication.
    pub fn load(
        gpu: &Arc<Gpu>,
        asset_manager: &AssetManager,
        constants: &SDynamicConstants,
    ) -> anyhow::Result<Self> {
        let (initial_constants, cbuffer) = if constants.constant_buffer.is_some() {
            let entry = package_manager()
                .get_entry(constants.constant_buffer)
                .context("Failed to get cbuffer tag entry")?;

            let data = package_manager().read_tag(entry.reference)?;
            let vec4s = bytemuck::cast_slice(&data);
            let cb = ConstantBuffer::create_array(gpu, vec4s.len(), Some(vec4s))?;
            (vec4s.to_vec(), Some(cb))
        } else {
            let vec4s = &constants.unk30;
            if vec4s.is_empty() {
                (vec![], None)
            } else {
                let cb = ConstantBuffer::create_array(gpu, vec4s.len(), Some(vec4s))?;
                (vec4s.to_vec(), Some(cb))
            }
        };

        let writes_cbuffer = OpcodeIterator::new(&constants.bytecode).any(|op| {
            matches!(
                op,
                (Opcode::PopOutput, _) | (Opcode::PopOutputMat4, _) | (Opcode::PushFromOutput, _)
            )
        });

        Ok(Self {
            textures: constants
                .textures
                .iter()
                .map(|tex| {
                    (
                        tex.slot,
                        asset_manager.try_load(tex.texture.hash32()),
                    )
                })
                .collect(),
            fallback_texture: gpu.placeholder_white.view.clone(),
            samplers: constants
                .samplers
                .iter()
                .map(|sampler| {
                    if sampler.sampler.is_none() {
                        debug!("TODO: Implement texture parameter access from expressions");
                        Ok(None)
                    } else {
                        let sampler = load_sampler(gpu, sampler.sampler)?;
                        Ok(Some(sampler))
                    }
                })
                .collect::<anyhow::Result<_>>()?,
            cbuffer_slot: constants.constant_buffer_slot as u32,
            cbuffer,
            bytecode: constants.bytecode.clone(),
            bytecode_constants: constants.bytecode_constants.clone(),

            initial_constants,

            writes_cbuffer,
        })
    }

    /// Shadowkeep serializes the same runtime concepts in a different order:
    /// bytecode is first and material textures use direct tag hashes.  Keep
    /// this decoder separate from the post-BL structure rather than casting
    /// one layout into the other.
    pub fn load_shadowkeep(
        gpu: &Arc<Gpu>,
        _asset_manager: &AssetManager,
        constants: &SShadowkeepDynamicConstants,
    ) -> anyhow::Result<Self> {
        let (initial_constants, cbuffer) = if constants.constant_buffer.is_some() {
            let entry = package_manager()
                .get_entry(constants.constant_buffer)
                .context("Failed to get Shadowkeep cbuffer tag entry")?;
            let data = package_manager().read_tag(entry.reference)?;
            let vec4s = bytemuck::cast_slice(&data);
            let cb = ConstantBuffer::create_array(gpu, vec4s.len(), Some(vec4s))?;
            (vec4s.to_vec(), Some(cb))
        } else if constants.inline_constants.is_empty() {
            (vec![], None)
        } else {
            let cb = ConstantBuffer::create_array(gpu, constants.inline_constants.len(), Some(&constants.inline_constants))?;
            (constants.inline_constants.clone(), Some(cb))
        };

        let bytecode = translate_shadowkeep_bytecode(&constants.bytecode)?;
        let writes_cbuffer = OpcodeIterator::new(&bytecode).any(|op| {
            matches!(
                op,
                (Opcode::PopOutput, _) | (Opcode::PopOutputMat4, _) | (Opcode::PushFromOutput, _)
            )
        });

        Ok(Self {
            // Shadowkeep places material assignments on the enclosing shader
            // record.  Scope constants have no texture assignment list.
            textures: Vec::new(),
            fallback_texture: gpu.placeholder_white.view.clone(),
            samplers: constants
                .samplers
                .iter()
                .map(|sampler| {
                    if sampler.sampler.is_none() {
                        Ok(None)
                    } else {
                        load_sampler(gpu, sampler.sampler).map(Some)
                    }
                })
                .collect::<anyhow::Result<_>>()?,
            cbuffer_slot: constants.constant_buffer_slot as u32,
            cbuffer,
            bytecode,
            bytecode_constants: constants.bytecode_constants.clone(),
            initial_constants,
            writes_cbuffer,
        })
    }

    #[profiling::function]
    fn prepare_constants(
        &self,
        cmd: &mut CommandList,
        channels: Option<&AHashMap<u32, ObjectChannel>>,
    ) -> anyhow::Result<()> {
        if self.writes_cbuffer {
            if let Some(ref cbuffer) = self.cbuffer {
                let map = unsafe {
                    cmd.map_unchecked(cbuffer.buffer(), 0, d3d11::MapType::WriteDiscard, false)?
                };
                let data = unsafe {
                    std::slice::from_raw_parts_mut(map.data as *mut Vec4, cbuffer.size() / 16)
                };

                // Copy the initial constants
                data[..self.initial_constants.len()].copy_from_slice(&self.initial_constants);
                self.evaluate_expressions(cmd, Some(data), channels);
                cmd.unmap(cbuffer.buffer(), 0);
            }
        } else {
            self.evaluate_expressions(cmd, None, channels);
        }

        Ok(())
    }

    fn evaluate_expressions(
        &self,
        cmd: &mut CommandList,
        output: Option<&mut [Vec4]>,
        channels: Option<&AHashMap<u32, ObjectChannel>>,
    ) {
        // profiling::scope!(
        //     "evaluate_expression_bytecode",
        //     &format!("bytes={}", self.bytecode.len())
        // );

        let mut interpreter = InterpreterState::new(&self.bytecode)
            .with_d3d11_context(cmd)
            .with_externs(&cmd.externs);
        if let Some(channels) = channels {
            interpreter = interpreter.with_object_channels(channels);
        }
        if let Err(e) = interpreter.evaluate(
            &self.bytecode_constants,
            &self.samplers,
            output.unwrap_or(&mut []),
        ) {
            error!("Failed to evaluate expression bytecode: {:?}", e);

            let bytecode_listing = match expression_vm::disassemble(&self.bytecode) {
                Ok(ops) => ops.into_iter().map(|v| format!("    {v}")).join("\n"),
                Err(e) => {
                    format!("Failed to disassemble bytecode: {e:?}")
                }
            };
            debug!("Bytecode:\n{}", bytecode_listing);

            if interpreter.ip < self.bytecode.len() {
                // Patch the bytecode to disable the expression
                unsafe {
                    self.bytecode
                        .as_ptr()
                        .add(interpreter.ip)
                        .cast_mut()
                        .write(expression_vm::opcodes::Opcode::ExtReturn as u8);
                }
            }
        }
    }

    #[profiling::function]
    pub fn bind(
        &self,
        cmd: &mut CommandList,
        stage: ShaderStage,
        channels: Option<&AHashMap<u32, ObjectChannel>>,
    ) -> anyhow::Result<()> {
        if !self.bytecode.is_empty() {
            self.prepare_constants(cmd, channels)?;
        }

        if self.cbuffer_slot != u32::MAX {
            if let Some(ref cbuffer) = self.cbuffer {
                cbuffer.bind(cmd, stage, self.cbuffer_slot);
            } else {
                cmd.set_constant_buffer(stage, self.cbuffer_slot, None);
            }
        }

        for &(slot, ref tex) in self.textures.iter() {
            if let Some(tex) = tex.as_ref().and_then(|t| t.get()) {
                tex.bind(cmd, slot, stage);
            } else {
                // Match the preserved renderer's fallback binding policy:
                // keep the slot valid while the asynchronous asset request
                // is queued or after a causal decode failure.  Required
                // failures remain visible through AssetManager diagnostics.
                cmd.set_shader_resource(stage, slot, &self.fallback_texture);
            }
        }

        Ok(())
    }
}

/// Converts the Arrivals expression bytecode dialect into the modern
/// interpreter dialect. The two clients share operation semantics but the
/// later client inserted and rearranged opcode values around the constant and
/// extern groups. Running legacy bytes directly makes a sampler load look like
/// an extern-matrix read, which in turn leaves material constant buffers
/// undefined and produces an empty G-buffer.
fn translate_shadowkeep_bytecode(source: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut translated = Vec::with_capacity(source.len());
    let mut cursor = 0;
    while cursor < source.len() {
        let legacy = source[cursor];
        let (current, size) = match legacy {
            // The base arithmetic block is stable through the legacy cubic
            // helper.  `0x0e` is still handled by the shared compatibility
            // implementation; its value did not move.
            0x01..=0x0f => (legacy, 1),
            0x10 => (0x13, 1), // lerp
            0x11 => (0x14, 1), // saturated lerp
            0x12 => (0x15, 1), // multiply-add
            0x13 => (0x16, 1), // clamp
            0x14 => (0x17, 1),
            0x15 => (0x18, 1), // abs
            0x16 => (0x19, 1), // signum
            0x17 => (0x1a, 1), // floor
            0x18 => (0x1b, 1), // ceil
            0x19 => (0x1c, 1), // round
            0x1a => (0x1d, 1), // frac
            0x1b => (0x1e, 1),
            0x1c => (0x1f, 1),
            0x1d => (0x20, 1), // negate
            0x1e => (0x21, 1), // vector sin
            0x1f => (0x22, 1), // vector cos
            0x20 => (0x23, 1), // vector sin/cos
            0x21 => (0x28, 1), // xxxx
            0x22 => (0x29, 2), // permute
            0x23 => (0x2a, 1), // saturate
            0x24 => (0x2b, 1),
            0x25 => (0x2c, 1),
            0x26 => (0x2d, 1),
            0x27 => (0x2e, 1), // triangle
            0x28 => (0x2f, 1), // jitter
            0x29 => (0x30, 1), // wander
            0x2a => (0x31, 1), // rand
            0x2b => (0x32, 1), // smooth rand
            0x2c => (0x33, 1),
            0x2d => (0x34, 1),
            0x2e => (0x35, 1), // transform vec4

            // Constant and extern instructions grew after Beyond Light.
            0x34 => (0x42, 2),
            0x35 => (0x43, 2),
            0x36 => (0x44, 2),
            0x37 => (0x45, 2),
            0x38 => (0x46, 2),
            0x39 => (0x47, 2),
            0x3a => (0x48, 2),
            0x3b => (0x49, 2),
            0x3c => (0x4a, 3),
            0x3d => (0x4b, 3),
            0x3e => (0x4c, 3),
            0x3f => (0x4d, 3),
            0x40 => (0x4e, 3),
            0x41 => (0x4f, 3),
            0x42 => (0x51, 2),
            0x43 => (0x52, 2),
            0x44 => (0x53, 2),
            0x45 => (0x54, 2),
            0x46 => (0x55, 2),
            0x47 => (0x56, 2),
            0x48 => (0x57, 2),
            0x49 => (0x58, 2),
            0x4a => (0x59, 2),
            0x4b => (0x5a, 2),
            0x4c => (0x5b, 2),
            0x4d => (0x5c, 2),
            0x4e => (0x5d, 2),
            _ => anyhow::bail!("unsupported Shadowkeep expression opcode 0x{legacy:02X} at 0x{cursor:X}"),
        };
        let end = cursor + size;
        anyhow::ensure!(end <= source.len(), "truncated Shadowkeep expression opcode 0x{legacy:02X} at 0x{cursor:X}");
        translated.push(current);
        if (0x3c..=0x41).contains(&legacy) {
            let extern_index = alkahest_data::tfx::shadowkeep::decode_extern_index(source[cursor + 1])
                .with_context(|| format!("invalid Shadowkeep extern index {} at 0x{cursor:X}", source[cursor + 1]))?;
            let mut offset = source[cursor + 2];
            match (extern_index, legacy) {
                // Arrivals' Deferred texture block begins at 0x38. The
                // normalized post-BL container begins at 0x78, so texture/UAV
                // operands move by eight 8-byte slots. Scalar 0x30 became
                // normalized scalar 0x70 and moves by sixteen 4-byte slots.
                (ExternIndex::Deferred, 0x3f | 0x41) => {
                    offset = offset.checked_add(8).context("Shadowkeep Deferred texture offset overflow")?;
                }
                (ExternIndex::Deferred, 0x3c) if offset == 0x0c => {
                    offset = 0x1c;
                }
                // View's preserved matrix block is compact; the normalized
                // post-BL View inserted one vec4 between each of these
                // matrices. Translate the legacy vec4 address rather than
                // adding overlapping Rust fields to the extern struct.
                (ExternIndex::View, 0x3e)
                    if matches!(offset, 0x06 | 0x0A | 0x0E | 0x12 | 0x1E | 0x26) =>
                {
                    offset = offset.checked_add(2).context("Shadowkeep View matrix offset overflow")?;
                }
                _ => {}
            }
            translated.extend_from_slice(&[source[cursor + 1], offset]);
        } else {
            translated.extend_from_slice(&source[cursor + 1..end]);
        }
        cursor = end;
    }
    Ok(translated)
}

#[cfg(test)]
mod tests {
    use super::{ExternIndex, translate_shadowkeep_bytecode};

    #[test]
    fn shadowkeep_bytecode_relocates_sampler_and_extern_opcodes() {
        assert_eq!(
            translate_shadowkeep_bytecode(&[0x4c, 0x00, 0x49, 0x21, 0x3e, 0x02, 0x03]).unwrap(),
            vec![0x5b, 0x00, 0x58, 0x21, 0x4c, 0x02, 0x03],
        );
    }

    #[test]
    fn shadowkeep_bytecode_relocates_legacy_extern_abi_offsets() {
        // Deferred texture slot 0x70 -> normalized 0xB0.
        assert_eq!(
            translate_shadowkeep_bytecode(&[0x3f, ExternIndex::Deferred as u8, 0x0e]).unwrap(),
            vec![0x4d, ExternIndex::Deferred as u8, 0x16],
        );
        // ObjectEffect's untyped legacy texture reads remain a placeholder;
        // the extern accessor handles them without moving the opcode.
        assert_eq!(
            translate_shadowkeep_bytecode(&[0x3f, 43, 0x02]).unwrap(),
            vec![0x4d, 43, 0x02],
        );
        // View camera_to_projective at legacy 0x60 -> normalized 0x80.
        assert_eq!(
            translate_shadowkeep_bytecode(&[0x3e, ExternIndex::View as u8, 0x06]).unwrap(),
            vec![0x4c, ExternIndex::View as u8, 0x08],
        );
        // The preserved fullscreen vertex pass reads projective_to_camera at
        // the compact legacy slot 0x0e.  The normalized View scope places it
        // at slot 0x10, alongside the other matrix relocations.
        assert_eq!(
            translate_shadowkeep_bytecode(&[0x3e, ExternIndex::View as u8, 0x0e]).unwrap(),
            vec![0x4c, ExternIndex::View as u8, 0x10],
        );
    }
}
