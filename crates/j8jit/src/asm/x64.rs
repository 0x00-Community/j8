use alloc::vec::Vec;

use super::{Alu, Cc, Label, Shift, R, RBP, RSP, SAVED, SAVED_END, X};
use crate::ir::FloatOp;

#[derive(Default)]
pub struct Asm {
    pub bytes: Vec<u8>,
    labels: Vec<Option<usize>>,
    fixups: Vec<(usize, Label)>,
}

impl Asm {
    pub fn label(&mut self) -> Label {
        self.labels.push(None);
        u32::try_from(self.labels.len() - 1).unwrap_or(0)
    }

    pub fn bind(&mut self, l: Label) {
        self.labels[l as usize] = Some(self.bytes.len());
    }

    pub fn here(&self) -> usize {
        self.bytes.len()
    }

    pub fn finish(&mut self) -> bool {
        for &(at, l) in &self.fixups {
            let Some(to) = self.labels[l as usize] else { return false };
            let rel = i32::try_from(to as isize - (at as isize + 4)).unwrap_or(0);
            self.bytes[at..at + 4].copy_from_slice(&rel.to_le_bytes());
        }
        true
    }

    fn b(&mut self, x: u8) {
        self.bytes.push(x);
    }

    fn rex(&mut self, w: bool, r: u8, x: u8, b: u8, force: bool) {
        let v = 0x40 | (u8::from(w) << 3) | ((r >> 3 & 1) << 2) | ((x >> 3 & 1) << 1) | (b >> 3 & 1);
        if v != 0x40 || force {
            self.b(v);
        }
    }

    fn mem(&mut self, reg: u8, base: R, disp: i32) {
        let rm = base & 7;
        let (md, small) = if disp == 0 && rm != 5 {
            (0u8, None)
        } else if let Ok(d) = i8::try_from(disp) {
            (1u8, Some(d))
        } else {
            (2u8, None)
        };
        self.b((md << 6) | ((reg & 7) << 3) | rm);
        if rm == 4 {
            self.b(0x24);
        }
        match (md, small) {
            (1, Some(d)) => self.b(d.cast_unsigned()),
            (2, _) => self.bytes.extend_from_slice(&disp.to_le_bytes()),
            _ => {}
        }
    }

    fn op_rm(&mut self, prefix: Option<u8>, w: bool, opcode: &[u8], reg: u8, base: R, disp: i32, byte_reg: bool) {
        if let Some(p) = prefix {
            self.b(p);
        }
        self.rex(w, reg, 0, base, byte_reg && reg >= 4);
        self.bytes.extend_from_slice(opcode);
        self.mem(reg, base, disp);
    }

    fn op_rr(&mut self, prefix: Option<u8>, w: bool, opcode: &[u8], reg: u8, rm: u8) {
        if let Some(p) = prefix {
            self.b(p);
        }
        self.rex(w, reg, 0, rm, false);
        self.bytes.extend_from_slice(opcode);
        self.b(0xc0 | ((reg & 7) << 3) | (rm & 7));
    }

    pub fn mov_rr(&mut self, dst: R, src: R) {
        if dst != src {
            self.op_rr(None, true, &[0x89], src, dst);
        }
    }

    pub fn mov_rr32(&mut self, dst: R, src: R) {
        self.op_rr(None, false, &[0x89], src, dst);
    }

    pub fn mov_ri(&mut self, dst: R, imm: u64) {
        if imm == 0 {
            self.op_rr(None, false, &[0x31], dst, dst);
        } else if let Ok(i) = u32::try_from(imm) {
            self.rex(false, 0, 0, dst, false);
            self.b(0xb8 + (dst & 7));
            self.bytes.extend_from_slice(&i.to_le_bytes());
        } else if let Ok(i) = i32::try_from(imm.cast_signed()) {
            self.rex(true, 0, 0, dst, false);
            self.b(0xc7);
            self.b(0xc0 | (dst & 7));
            self.bytes.extend_from_slice(&i.to_le_bytes());
        } else {
            self.rex(true, 0, 0, dst, false);
            self.b(0xb8 + (dst & 7));
            self.bytes.extend_from_slice(&imm.to_le_bytes());
        }
    }

    pub fn load(&mut self, dst: R, base: R, disp: i32) {
        self.op_rm(None, true, &[0x8b], dst, base, disp, false);
    }

    pub fn load32(&mut self, dst: R, base: R, disp: i32) {
        self.op_rm(None, false, &[0x8b], dst, base, disp, false);
    }

    pub fn store(&mut self, base: R, disp: i32, src: R) {
        self.op_rm(None, true, &[0x89], src, base, disp, false);
    }

    pub fn lea(&mut self, dst: R, base: R, disp: i32) {
        self.op_rm(None, true, &[0x8d], dst, base, disp, false);
    }

    pub fn alu_rr(&mut self, op: Alu, w: bool, dst: R, src: R) {
        self.op_rr(None, w, &[(op as u8) << 3 | 1], src, dst);
    }

    pub fn alu_ri(&mut self, op: Alu, w: bool, dst: R, imm: i32) {
        if let Ok(i) = i8::try_from(imm) {
            self.op_rr(None, w, &[0x83], op as u8, dst);
            self.b(i.cast_unsigned());
        } else {
            self.op_rr(None, w, &[0x81], op as u8, dst);
            self.bytes.extend_from_slice(&imm.to_le_bytes());
        }
    }

    pub fn alu_rm(&mut self, op: Alu, w: bool, dst: R, base: R, disp: i32) {
        self.op_rm(None, w, &[(op as u8) << 3 | 3], dst, base, disp, false);
    }

    pub fn cmp_mi(&mut self, w: bool, base: R, disp: i32, imm: i32) {
        self.op_rm(None, w, &[0x81], 7, base, disp, false);
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    pub fn cmp_m8i(&mut self, base: R, disp: i32, imm: u8) {
        self.op_rm(None, false, &[0x80], 7, base, disp, false);
        self.b(imm);
    }

    pub fn test_m8i(&mut self, base: R, disp: i32, imm: u8) {
        self.op_rm(None, false, &[0xf6], 0, base, disp, false);
        self.b(imm);
    }

    pub fn test_rr(&mut self, w: bool, a: R, b: R) {
        self.op_rr(None, w, &[0x85], b, a);
    }

    pub fn imul_rr(&mut self, w: bool, dst: R, src: R) {
        self.op_rr(None, w, &[0x0f, 0xaf], dst, src);
    }

    pub fn imul_rri(&mut self, w: bool, dst: R, src: R, imm: i32) {
        self.op_rr(None, w, &[0x69], dst, src);
        self.bytes.extend_from_slice(&imm.to_le_bytes());
    }

    pub fn cdq_idiv(&mut self, src: R) {
        self.b(0x99);
        self.op_rr(None, false, &[0xf7], 7, src);
    }

    pub fn shift_cl(&mut self, kind: Shift, w: bool, dst: R) {
        self.op_rr(None, w, &[0xd3], kind as u8, dst);
    }

    pub fn shift_ri(&mut self, kind: Shift, w: bool, dst: R, n: u8) {
        self.op_rr(None, w, &[0xc1], kind as u8, dst);
        self.b(n);
    }

    pub fn movsxd(&mut self, dst: R, src: R) {
        self.op_rr(None, true, &[0x63], dst, src);
    }

    pub fn setcc(&mut self, cc: Cc, dst: R) {
        self.rex(false, 0, 0, dst, dst >= 4);
        self.b(0x0f);
        self.b(0x90 | cc as u8);
        self.b(0xc0 | (dst & 7));
        self.rex(false, dst, 0, dst, dst >= 4);
        self.b(0x0f);
        self.b(0xb6);
        self.b(0xc0 | ((dst & 7) << 3) | (dst & 7));
    }

    pub fn cmov(&mut self, cc: Cc, dst: R, src: R) {
        self.op_rr(None, true, &[0x0f, 0x40 | cc as u8], dst, src);
    }

    pub fn jcc(&mut self, cc: Cc, l: Label) {
        self.b(0x0f);
        self.b(0x80 | cc as u8);
        let at = self.here();
        self.bytes.extend_from_slice(&[0; 4]);
        self.fixups.push((at, l));
    }

    pub fn jmp(&mut self, l: Label) {
        self.b(0xe9);
        let at = self.here();
        self.bytes.extend_from_slice(&[0; 4]);
        self.fixups.push((at, l));
    }

    pub fn call_abs(&mut self, target: u64) -> usize {
        self.b(0x48);
        self.b(0xb8);
        let at = self.here();
        self.bytes.extend_from_slice(&target.to_le_bytes());
        self.b(0xff);
        self.b(0xd0);
        at
    }

    pub fn enter(&mut self, frame: i32) {
        self.push(RBP);
        self.mov_rr(RBP, RSP);
        for r in SAVED {
            self.push(r);
        }
        self.alu_ri(Alu::Sub, true, RSP, frame);
    }

    pub fn leave(&mut self) {
        self.lea(RSP, RBP, SAVED_END);
        for r in SAVED.into_iter().rev() {
            self.pop(r);
        }
        self.pop(RBP);
        self.ret();
    }

    pub fn save(&mut self, regs: &[R]) {
        for &r in regs {
            self.push(r);
        }
        if regs.len() % 2 == 1 {
            self.alu_ri(Alu::Sub, true, RSP, 8);
        }
    }

    pub fn restore(&mut self, regs: &[R]) {
        if regs.len() % 2 == 1 {
            self.alu_ri(Alu::Add, true, RSP, 8);
        }
        for &r in regs.iter().rev() {
            self.pop(r);
        }
    }

    fn push(&mut self, r: R) {
        self.rex(false, 0, 0, r, false);
        self.b(0x50 + (r & 7));
    }

    fn pop(&mut self, r: R) {
        self.rex(false, 0, 0, r, false);
        self.b(0x58 + (r & 7));
    }

    fn ret(&mut self) {
        self.b(0xc3);
    }

    pub fn movq_xr(&mut self, x: X, r: R) {
        self.op_rr(Some(0x66), true, &[0x0f, 0x6e], x, r);
    }

    pub fn movq_rx(&mut self, r: R, x: X) {
        self.op_rr(Some(0x66), true, &[0x0f, 0x7e], x, r);
    }

    pub fn movsd_xx(&mut self, dst: X, src: X) {
        if dst != src {
            self.op_rr(Some(0xf2), false, &[0x0f, 0x10], dst, src);
        }
    }

    pub fn movsd_load(&mut self, x: X, base: R, disp: i32) {
        self.op_rm(Some(0xf2), false, &[0x0f, 0x10], x, base, disp, false);
    }

    pub fn movsd_store(&mut self, base: R, disp: i32, x: X) {
        self.op_rm(Some(0xf2), false, &[0x0f, 0x11], x, base, disp, false);
    }

    pub fn float(&mut self, op: FloatOp, dst: X, src: X) {
        let code = match op {
            FloatOp::Add => 0x58,
            FloatOp::Mul => 0x59,
            FloatOp::Sub => 0x5c,
            FloatOp::Div => 0x5e,
        };
        self.op_rr(Some(0xf2), false, &[0x0f, code], dst, src);
    }

    pub fn ucomisd(&mut self, a: X, b: X) {
        self.op_rr(Some(0x66), false, &[0x0f, 0x2e], a, b);
    }

    pub fn cvtsi2sd(&mut self, x: X, r: R) {
        self.op_rr(Some(0xf2), false, &[0x0f, 0x2a], x, r);
    }

    pub fn cvttsd2si(&mut self, r: R, x: X) {
        self.op_rr(Some(0xf2), false, &[0x0f, 0x2c], r, x);
    }

    pub fn cvtsi2sd64(&mut self, x: X, r: R) {
        self.op_rr(Some(0xf2), true, &[0x0f, 0x2a], x, r);
    }

    pub fn cvttsd2si64(&mut self, r: R, x: X) {
        self.op_rr(Some(0xf2), true, &[0x0f, 0x2c], r, x);
    }

    pub fn cqo_idiv(&mut self, src: R) {
        self.b(0x48);
        self.b(0x99);
        self.op_rr(None, true, &[0xf7], 7, src);
    }

    pub fn zero_x(&mut self, x: X) {
        self.op_rr(Some(0x66), false, &[0x0f, 0x57], x, x);
    }
}
