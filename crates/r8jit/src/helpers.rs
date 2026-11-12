use r8compiler::Helper;
use r8framework::BinaryOp;

/// 机器码调的运行时函数（`extern "C"`，地址填进机器码）。
pub(crate) struct Helpers;

/// 两个数做二元运算 `OP`（[`BinaryOp`] 的下标），与解释器同一份实现
extern "C" fn number<const OP: usize>(a: f64, b: f64) -> f64 {
    Helpers::OPS[OP].numbers(a, b)
}

impl Helpers {
    /// 运行时函数按下标找运算
    const OPS: [BinaryOp; 8] = [BinaryOp::Rem, BinaryOp::Exp, BinaryOp::BitAnd, BinaryOp::BitOr, BinaryOp::BitXor, BinaryOp::Shl, BinaryOp::Shr, BinaryOp::UShr];

    /// 运行时函数 `h` 的地址（四则运算机器码里直接算，没有运行时函数）。
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
