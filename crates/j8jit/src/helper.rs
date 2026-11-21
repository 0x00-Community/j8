use j8framework::BinaryOp;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Helper {
    Number(BinaryOp),
}

impl Helper {
    const TABLE: [(Helper, &'static str); 8] = [
        (Helper::Number(BinaryOp::Rem), "j8_number_rem"),
        (Helper::Number(BinaryOp::Exp), "j8_number_exp"),
        (Helper::Number(BinaryOp::BitAnd), "j8_number_bitand"),
        (Helper::Number(BinaryOp::BitOr), "j8_number_bitor"),
        (Helper::Number(BinaryOp::BitXor), "j8_number_bitxor"),
        (Helper::Number(BinaryOp::Shl), "j8_number_shl"),
        (Helper::Number(BinaryOp::Shr), "j8_number_shr"),
        (Helper::Number(BinaryOp::UShr), "j8_number_ushr"),
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
