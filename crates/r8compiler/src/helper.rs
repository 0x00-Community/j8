use r8framework::BinaryOp;

/// Runtime functions called from machine code (addresses filled in by the loader; see
/// [`Native::relocs`](crate::Native::relocs)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Helper {
    /// `extern "C" fn(f64, f64) -> f64`: applies this binary operation to two numbers (ops other than the four
    /// arithmetic ones: remainder, exponentiation, bitwise ops)
    Number(BinaryOp),
}

impl Helper {
    /// All runtime functions and their symbol names in MLIR (each name appears only here).
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

    /// Symbol name in MLIR; the four arithmetic ops don't go through runtime functions (`None`).
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
