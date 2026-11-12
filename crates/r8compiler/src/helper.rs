use r8framework::BinaryOp;

/// 机器码调的运行时函数（地址由装载方填，见 [`Native::relocs`](crate::Native::relocs)）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Helper {
    /// `extern "C" fn(f64, f64) -> f64`：两个数做这个二元运算（四则以外的：取余、乘方、位运算）
    Number(BinaryOp),
}

impl Helper {
    /// 全部运行时函数与它们在 MLIR 里的符号名（名字只在这里出现一次）。
    const TABLE: [(Helper, &'static str); 8] = [
        (Helper::Number(BinaryOp::Rem), "r8_number_rem"),
        (Helper::Number(BinaryOp::Exp), "r8_number_exp"),
        (Helper::Number(BinaryOp::BitAnd), "r8_number_bitand"),
        (Helper::Number(BinaryOp::BitOr), "r8_number_bitor"),
        (Helper::Number(BinaryOp::BitXor), "r8_number_bitxor"),
        (Helper::Number(BinaryOp::Shl), "r8_number_shl"),
        (Helper::Number(BinaryOp::Shr), "r8_number_shr"),
        (Helper::Number(BinaryOp::UShr), "r8_number_ushr"),
    ];

    pub const ALL: [Helper; Self::TABLE.len()] = {
        let mut all = [Helper::Number(BinaryOp::Rem); Self::TABLE.len()];
        let mut i = 0;
        while i < all.len() {
            all[i] = Self::TABLE[i].0;
            i += 1;
        }
        all
    };

    /// 在 MLIR 里的符号名；四则运算不经运行时函数（给 `None`）。
    pub fn symbol(self) -> Option<&'static str> {
        Self::TABLE.iter().find(|(h, _)| *h == self).map(|(_, s)| *s)
    }
}

impl TryFrom<&str> for Helper {
    type Error = ();

    fn try_from(s: &str) -> Result<Self, ()> {
        Self::TABLE.iter().find(|(_, n)| *n == s).map(|(h, _)| *h).ok_or(())
    }
}
