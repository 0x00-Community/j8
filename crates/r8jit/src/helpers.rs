use r8compiler::Helper;
use r8framework::BinaryOp;

/// Runtime functions called from machine code (`extern "C"`, addresses patched into the machine code).
pub(crate) struct Helpers;

/// Binary operation `OP` (an index into [`BinaryOp`]) on two numbers; same implementation as the interpreter
extern "C" fn number<const OP: usize>(a: f64, b: f64) -> f64 {
    Helpers::OPS[OP].numbers(a, b)
}

impl Helpers {
    /// The operation of each runtime function, by index
    const OPS: [BinaryOp; 8] = [BinaryOp::Rem, BinaryOp::Exp, BinaryOp::BitAnd, BinaryOp::BitOr, BinaryOp::BitXor, BinaryOp::Shl, BinaryOp::Shr, BinaryOp::UShr];

    /// Address of runtime function `h` (the four arithmetic ops are computed directly in machine code and have no
    /// runtime function).
    pub(crate) fn address(h: Helper) -> Option<usize> {
        let Helper::Number(op) = h;
        let f: extern "C" fn(f64, f64) -> f64 = match op {
            BinaryOp::Rem => number::<0>,
            BinaryOp::Exp => number::<1>,
            BinaryOp::BitAnd => number::<2>,
            BinaryOp::BitOr => number::<3>,
            BinaryOp::BitXor => number::<4>,
            BinaryOp::Shl => number::<5>,
            BinaryOp::Shr => number::<6>,
            BinaryOp::UShr => number::<7>,
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div => return None,
        };
        Some(f as usize)
    }
}
