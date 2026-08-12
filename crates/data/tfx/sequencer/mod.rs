use glam::Vec4;
use int_enum::IntEnum;
use tiger_parse::{
    tiger_type, tiger_variant_enum, FnvHash, TigerReadable, TigerReader, VariantPointer,
};
use tiger_pkg::TagHash;

use crate::tag::{OptionalTag, WideHash};

#[derive(Debug, Clone)]
#[tiger_type(id = 0x80808179, size = 0x220)]
pub struct SSequence {
    #[tiger(offset = 0x1C8)]
    pub m_flow_nodes: Vec<SUnk808091f1>,
    pub m_work_nodes: Vec<SUnk808091f1>,
    // pub unk1e8: Vec<SUnk808084df>,
    #[tiger(offset = 0x1F8)]
    pub m_channel_providers: Vec<SUnk8080816f>,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x8080816f, size = 0x38)]
pub struct SUnk8080816f {
    pub unk0: TagHash,
    pub unk4: u32,
    pub unk8: i64,
    pub unk10: i64,
    pub unk18: TagHash,
    #[tiger(offset = 0x30)]
    pub unk30: FnvHash,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x808091f1, size = 0x18)]
pub struct SUnk808091f1 {
    #[tiger(offset = 0x10)]
    pub owner_pointer: VariantPointer<SUnk808091f1Variant>,
}

tiger_variant_enum! {
    #[derive(Debug, Clone)]
    [Unknown(true)]
    enum SUnk808091f1Variant {
        SSequenceGlobalChannel,
        SSequenceFlowParallel,
        SUnk808091d9,
        SUnk808091db,
        SUnk808091dd,
        SUnk808091df,
        SUnk808091e1,
        SUnk808091e5,
        SSequenceDelay,
        SSequenceDamageImpulse,
        SSequenceAreaImpulse,
        SSequenceScreenAreaFx,
        SSequenceLight,
        SSequenceLensFlare,
        SSequenceEmbeddedParticleSystem,
        SSequenceAudioEvent,
        SUnk80802636Animation
    }
}

impl SUnk808091f1Variant {
    pub fn base(&self) -> Option<&SSequenceNodeBase> {
        match self {
            SUnk808091f1Variant::SSequenceGlobalChannel(n) => Some(&n.base),
            SUnk808091f1Variant::SSequenceFlowParallel(n) => Some(&n.base),
            SUnk808091f1Variant::SUnk808091d9(n) => Some(&n.base),
            SUnk808091f1Variant::SUnk808091db(n) => Some(&n.base),
            SUnk808091f1Variant::SUnk808091dd(n) => Some(&n.base),
            SUnk808091f1Variant::SUnk808091df(n) => Some(&n.base),
            SUnk808091f1Variant::SUnk808091e1(n) => Some(&n.base),
            SUnk808091f1Variant::SUnk808091e5(n) => Some(&n.base),
            SUnk808091f1Variant::SSequenceDelay(n) => Some(&n.base),
            SUnk808091f1Variant::SSequenceDamageImpulse(n) => Some(&n.base),
            SUnk808091f1Variant::SSequenceAreaImpulse(n) => Some(&n.base),
            SUnk808091f1Variant::SSequenceScreenAreaFx(n) => Some(&n.base),
            SUnk808091f1Variant::SSequenceLight(n) => Some(&n.base),
            SUnk808091f1Variant::SSequenceLensFlare(n) => Some(&n.base),
            SUnk808091f1Variant::SSequenceEmbeddedParticleSystem(n) => Some(&n.base),
            SUnk808091f1Variant::SSequenceAudioEvent(n) => Some(&n.base),
            SUnk808091f1Variant::SUnk80802636Animation(n) => Some(&n.base),
            SUnk808091f1Variant::Unknown { .. } => None,
        }
    }
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x808091d1, size = 0x70)]
pub struct SSequenceGlobalChannel {
    pub base: SSequenceNodeBase,
    pub unk20: u32,
    pub unk24: u32,
    pub other_index: u32,
    pub unk2c: FnvHash,

    pub bytecode: Vec<u8>,
    pub bytecode_constants: Vec<Vec4>,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x808091e3, size = 0x60)]
pub struct SSequenceFlowParallel {
    pub base: SSequenceNodeBase,
    pub children: Vec<SSequenceNodeRef>,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x808091dd, size = 0x60)]
pub struct SUnk808091dd {
    pub base: SSequenceNodeBase,
    pub children: Vec<SSequenceNodeRef>,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x808091d9, size = 0x58)]
pub struct SUnk808091d9 {
    pub base: SSequenceNodeBase,
    pub children: Vec<SSequenceNodeRef>,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x808091df, size = 0x60)]
pub struct SUnk808091df {
    pub base: SSequenceNodeBase,
    pub children: Vec<SSequenceNodeRef>,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x808091e5, size = 0x40)]
pub struct SUnk808091e5 {
    pub base: SSequenceNodeBase,
    pub children: Vec<SSequenceNodeRef>,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x808091db, size = 0x60)]
pub struct SUnk808091db {
    pub base: SSequenceNodeBase,
    pub children: Vec<SSequenceNodeRef>,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x808091e1, size = 0x50)]
pub struct SUnk808091e1 {
    pub base: SSequenceNodeBase,
    pub children: Vec<SSequenceNodeRef>,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x80802636, size = 0x1A0)]
pub struct SUnk80802636Animation {
    pub base: SSequenceNodeBase,

    #[tiger(offset = 0x28)]
    pub layer_weight: Vec<()>,

    #[tiger(offset = 0x68)]
    pub unk68: SUnk80809205,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x808091D7, size = 0x20)]
pub struct SSequenceDelay {
    pub base: SSequenceNodeBase,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x80803F2F, size = 0x190)]
pub struct SSequenceDamageImpulse {
    pub base: SSequenceNodeBase,

    #[tiger(offset = 0x40)]
    pub unk_3692abc1: SUnk80809205,
    #[tiger(offset = 0x80)]
    pub unk_832a631d: SUnk80809205,
    #[tiger(offset = 0xC0)]
    pub unk_c2983c91: SUnk80809205,
    #[tiger(offset = 0x100)]
    pub unk_6843cbea: SUnk80809205,
    #[tiger(offset = 0x140)]
    pub unk_69060ef6: SUnk80809205,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x80803F24, size = 0x510)]
pub struct SSequenceAreaImpulse {
    pub base: SSequenceNodeBase,

    // #[tiger(offset = 0x28)]
    // pub area_shape_query: SUnk8080913D,
    // #[tiger(offset = 0x90)]
    // pub unk0x1B8FE84B: SUnk80809080,
    #[tiger(offset = 0xC0)]
    pub unk0x1cf4038f: SUnk80809205,
    #[tiger(offset = 0x100)]
    pub unk0x98ddb465: SUnk80809205,
    // #[tiger(offset = 0x140)]
    // pub unk0x1E06BDA4: SUnk80803F26,
    // #[tiger(offset = 0x148)]
    // pub unk0x3B2FE65F: SUnk8080849A,
    // #[tiger(offset = 0x158)]
    // pub unk0x3969DEA0: SUnk80803F25,
    // #[tiger(offset = 0x178)]
    // pub unk0xE2A451E9: SUnk80809080,
    // #[tiger(offset = 0x1C0)]
    // pub attacker_responses: SUnk80803F79,
    // #[tiger(offset = 0x1D0)]
    // pub unk0xA57C803A: SUnk80802FFF,
    #[tiger(offset = 0x4C8)]
    pub unk0x1d974d72: SUnk80809205,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x808091CF, size = 0x100)]
pub struct SSequenceScreenAreaFx {
    pub base: SSequenceNodeBase,

    #[tiger(offset = 0x30)]
    pub unk30: SUnk80809205,

    #[tiger(offset = 0x70)]
    pub unk70: SUnk80809204,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x80809205, size = 0x40)]
pub struct SUnk80809205 {
    // expression 808095CB 0x0
    // 0x45C37927 80809F08 0x30
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x80809204, size = 0x90)]
pub struct SUnk80809204 {
    // fade_in_curve 80809205 @ 0x8
    // fade_out_curve 80809205 @ 0x50
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x80806a52, size = 0x130)]
pub struct SSequenceLight {
    pub base: SSequenceNodeBase,
    pub unk20: u32,
    pub light: TagHash,
    pub unk28: u32,
    pub unk2c: u32,
    pub unk30: Vec4,
    pub unk40: Vec4,
    pub unk50: u64,

    pub unk58: SUnknownEventExpressions,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x80806a48, size = 0x130)]
pub struct SSequenceLensFlare {
    pub base: SSequenceNodeBase,
    pub unk20: u32,
    pub flare: TagHash,

    #[tiger(offset = 0x40)]
    pub unk40: SUnknownEventExpressions,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x80806640, size = 0x60)]
pub struct SSequenceAudioEvent {
    pub base: SSequenceNodeBase,
    #[tiger(offset = 0x50)]
    pub wwise_event: WideHash,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x808067b9, size = 0x110)]
pub struct SSequenceEmbeddedParticleSystem {
    pub base: SSequenceNodeBase,

    #[tiger(offset = 0x28)]
    pub unk28: Vec<SUnk808067bb>,
    pub unk38: SUnk80809205,
    pub unk78: SUnk80809204,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x808067bb, size = 0x18)]
pub struct SUnk808067bb {
    pub unk0: Vec<u8>,
    pub particle_system: OptionalTag<SUnk80806920>,
    pub unk14: TagHash,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x00000000, size = 0xd8)]
pub struct SUnknownEventExpressions {
    pub unk00: SExpression,

    #[tiger(offset = 0x48)]
    pub unk48: SExpression,

    #[tiger(offset = 0x90)]
    pub unk88: SExpression,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x00000000)]
pub struct SSequenceNodeBase {
    pub name: FnvHash,
    pub unk4: u16,
    pub parent_index: u16,
    pub unk8: u32,

    pub unkc: f32,
    pub start_time: f32,
    pub unk14: f32,
    pub duration: f32,
    pub unk1c: u32,
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x00000000, size = 0x30)]
pub struct SExpression {
    pub bytecode: Vec<u8>,
    pub bytecode_constants: Vec<Vec4>,
    pub unk20: u64,
    pub unk28: u64,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
#[tiger_type(id = 0x808094E9, size = 4)]
pub struct SSequenceNodeRef {
    pub kind: NodeKind,
    pub index: u16,
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, IntEnum, Hash, PartialEq, Eq)]
pub enum NodeKind {
    Flow = 0,
    Work = 1,
}

impl TigerReadable for NodeKind {
    fn read_ds_endian(
        reader: &mut dyn TigerReader,
        endian: tiger_parse::Endian,
    ) -> tiger_parse::Result<Self> {
        let v = u16::read_ds_endian(reader, endian)?;
        Self::try_from(v).map_err(|_| tiger_parse::Error::EnumVariantOutOfRange(v as usize))
    }

    const SIZE: usize = 2;
}

#[derive(Debug, Clone)]
#[tiger_type(id = 0x80806920, size = 0x48)]
pub struct SUnk80806920 {
    pub unk0: TagHash,
    pub unk4: TagHash,
    pub unk8_compute_technique: TagHash,
    pub unkc_compute_technique: TagHash,
    pub unk10: u32,
    pub unk14_compute_technique: TagHash,
    pub unk18_raster_technique: TagHash,
}

impl SUnk80806920 {
    pub fn is_gpu(&self) -> bool {
        self.unk8_compute_technique.is_some()
            || self.unkc_compute_technique.is_some()
            || self.unk14_compute_technique.is_some()
    }
}
