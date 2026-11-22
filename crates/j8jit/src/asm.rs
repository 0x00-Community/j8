pub type R = u8;

pub type X = u8;

pub const RAX: R = 0;
pub const RCX: R = 1;
pub const RDX: R = 2;
pub const RBX: R = 3;
pub const RSP: R = 4;
pub const RBP: R = 5;
pub const RSI: R = 6;
pub const RDI: R = 7;
pub const R8: R = 8;
pub const R9: R = 9;
pub const R10: R = 10;
pub const R11: R = 11;
pub const R12: R = 12;
pub const R13: R = 13;
pub const R14: R = 14;
pub const R15: R = 15;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cc {
    O = 0,
    No = 1,
    B = 2,
    Ae = 3,
    E = 4,
    Ne = 5,
    Be = 6,
    A = 7,
    S = 8,
    Ns = 9,
    P = 0xa,
    Np = 0xb,
    L = 0xc,
    Ge = 0xd,
    Le = 0xe,
    G = 0xf,
}

impl Cc {
    pub fn negate(self) -> Cc {
        match self {
            Cc::O => Cc::No,
            Cc::No => Cc::O,
            Cc::B => Cc::Ae,
            Cc::Ae => Cc::B,
            Cc::E => Cc::Ne,
            Cc::Ne => Cc::E,
            Cc::Be => Cc::A,
            Cc::A => Cc::Be,
            Cc::S => Cc::Ns,
            Cc::Ns => Cc::S,
            Cc::P => Cc::Np,
            Cc::Np => Cc::P,
            Cc::L => Cc::Ge,
            Cc::Ge => Cc::L,
            Cc::Le => Cc::G,
            Cc::G => Cc::Le,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alu {
    Add = 0,
    Or = 1,
    And = 4,
    Sub = 5,
    Xor = 6,
    Cmp = 7,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shift {
    Shl = 4,
    Shr = 5,
    Sar = 7,
}

pub type Label = u32;

pub const SAVED: [R; 5] = [RBX, R12, R13, R14, R15];

pub const SAVED_END: i32 = -40;

#[cfg(not(target_arch = "aarch64"))]
mod x64;
#[cfg(not(target_arch = "aarch64"))]
pub use x64::Asm;

#[cfg(target_arch = "aarch64")]
mod a64;
#[cfg(target_arch = "aarch64")]
pub use a64::Asm;
