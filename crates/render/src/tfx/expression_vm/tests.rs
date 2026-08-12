use std::panic::{AssertUnwindSafe, catch_unwind};

use glam::Vec4;

use super::{interpreter::InterpreterState, opcodes::Opcode};

fn evaluate(bytecode: &[u8], constants: &[Vec4], out: &mut [Vec4]) -> anyhow::Result<()> {
    InterpreterState::new(bytecode).evaluate(constants, &[], out)
}

#[test]
fn simple_program_adds_constants() {
    let bytecode = [
        Opcode::PushConstVec4 as u8,
        0,
        Opcode::PushConstVec4 as u8,
        1,
        Opcode::Add as u8,
        Opcode::PopOutput as u8,
        0,
        Opcode::ExtReturn as u8,
    ];
    let constants = [Vec4::new(1.0, 2.0, 3.0, 4.0), Vec4::splat(5.0)];
    let mut out = [Vec4::ZERO];

    evaluate(&bytecode, &constants, &mut out).unwrap();

    assert_eq!(out[0], Vec4::new(6.0, 7.0, 8.0, 9.0));
}

#[test]
fn arbitrary_short_bytecode_never_panics() {
    for opcode in 0..=u8::MAX {
        for len in 1..=5 {
            let mut bytecode = vec![0xA5; len];
            bytecode[0] = opcode;
            let result = catch_unwind(AssertUnwindSafe(|| {
                let mut out = [Vec4::ZERO; 4];
                let _ = evaluate(&bytecode, &[], &mut out);
            }));
            assert!(result.is_ok(), "opcode 0x{opcode:02X}, length {len}");
        }
    }
}

#[test]
fn stack_boundaries_fail_closed() {
    let mut overflow = Vec::new();
    for _ in 0..33 {
        overflow.extend([Opcode::PushConstVec4 as u8, 0]);
    }
    overflow.push(Opcode::ExtReturn as u8);

    let mut out = [Vec4::ZERO];
    assert!(evaluate(&overflow, &[Vec4::ONE], &mut out).is_err());
    assert!(evaluate(&[Opcode::Add as u8], &[], &mut out).is_err());
    assert!(evaluate(&[Opcode::PushObjectChannelVector as u8], &[], &mut out).is_err());
}
