use j8framework::{Agent, Error, Value};

use crate::{Args, Installer};

struct Math;

type Unary = fn(f64) -> f64;

const UNARY: [(&str, Unary); 19] = [
    ("abs", libm::fabs),
    ("floor", libm::floor),
    ("ceil", libm::ceil),
    ("trunc", libm::trunc),
    ("sqrt", libm::sqrt),
    ("cbrt", libm::cbrt),
    ("exp", libm::exp),
    ("expm1", libm::expm1),
    ("log", libm::log),
    ("log2", libm::log2),
    ("log10", libm::log10),
    ("log1p", libm::log1p),
    ("sin", libm::sin),
    ("cos", libm::cos),
    ("tan", libm::tan),
    ("asin", libm::asin),
    ("acos", libm::acos),
    ("atan", libm::atan),
    ("sign", Math::sign),
];

impl Math {
    fn sign(x: f64) -> f64 {
        if x.is_nan() || x == 0.0 { x } else { x.signum() }
    }

    fn unary<'gc, const N: usize>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok((UNARY[N].1)(a.to_number(args.at(0))?).into())
    }

    fn unary1<'gc, const N: usize>(_: &mut Agent<'gc, '_>, _: Value<'gc>, v: Value<'gc>) -> Option<Value<'gc>> {
        match v.kind() {
            j8framework::ValueKind::Number(x) => Some((UNARY[N].1)(x).into()),
            _ => None,
        }
    }

    fn extreme2<'gc>(a: &mut Agent<'gc, '_>, x: Value<'gc>, y: Value<'gc>, max: bool) -> Option<Value<'gc>> {
        if !matches!((x.kind(), y.kind()), (j8framework::ValueKind::Number(_), j8framework::ValueKind::Number(_))) {
            return None;
        }
        Self::extreme(a, &[x, y], max).ok()
    }

    fn max2<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, x: Value<'gc>, y: Value<'gc>) -> Option<Value<'gc>> {
        Self::extreme2(a, x, y, true)
    }

    fn min2<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, x: Value<'gc>, y: Value<'gc>) -> Option<Value<'gc>> {
        Self::extreme2(a, x, y, false)
    }

    fn round<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let x = a.to_number(args.at(0))?;
        let f = libm::floor(x);
        Ok((if x - f >= 0.5 { f + 1.0 } else { f }).into())
    }

    fn pow<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (x, y) = (args.at(0), args.at(1));
        a.binary(j8framework::BinaryOp::Exp, x, y)
    }

    fn atan2<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(libm::atan2(a.to_number(args.at(0))?, a.to_number(args.at(1))?).into())
    }

    fn extreme<'gc>(a: &mut Agent<'gc, '_>, args: &[Value<'gc>], max: bool) -> Result<Value<'gc>, Error> {
        let mut acc = if max { f64::NEG_INFINITY } else { f64::INFINITY };
        for &v in args {
            let x = a.to_number(v)?;
            let better = if x.is_nan() || acc.is_nan() {
                true
            } else if x == acc {
                (x == 0.0) && (x.is_sign_positive() == max)
            } else {
                (x > acc) == max
            };
            if better {
                acc = if acc.is_nan() { acc } else { x };
            }
        }
        Ok(acc.into())
    }

    fn max<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::extreme(a, args, true)
    }

    fn min<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::extreme(a, args, false)
    }

    fn hypot<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let mut sum = 0.0;
        for &v in args {
            let x = a.to_number(v)?;
            sum += x * x;
        }
        Ok(libm::sqrt(sum).into())
    }
}

impl Installer<'_, '_> {
    pub(crate) fn math(&mut self) {
        let m = self.plain();
        let unary: [j8framework::NativeFn; UNARY.len()] = [
            Math::unary::<0>,
            Math::unary::<1>,
            Math::unary::<2>,
            Math::unary::<3>,
            Math::unary::<4>,
            Math::unary::<5>,
            Math::unary::<6>,
            Math::unary::<7>,
            Math::unary::<8>,
            Math::unary::<9>,
            Math::unary::<10>,
            Math::unary::<11>,
            Math::unary::<12>,
            Math::unary::<13>,
            Math::unary::<14>,
            Math::unary::<15>,
            Math::unary::<16>,
            Math::unary::<17>,
            Math::unary::<18>,
        ];
        for ((name, _), f) in UNARY.iter().zip(unary) {
            self.method(m, name, f, 1);
        }
        self.direct(Math::unary::<0>, j8framework::FastCall::A1(Math::unary1::<0>), j8framework::FastOut::Number);
        self.direct(Math::unary::<1>, j8framework::FastCall::A1(Math::unary1::<1>), j8framework::FastOut::Number);
        self.direct(Math::unary::<2>, j8framework::FastCall::A1(Math::unary1::<2>), j8framework::FastOut::Number);
        self.direct(Math::unary::<4>, j8framework::FastCall::A1(Math::unary1::<4>), j8framework::FastOut::Number);
        self.method(m, "round", Math::round, 1);
        self.method(m, "pow", Math::pow, 2);
        self.method(m, "atan2", Math::atan2, 2);
        self.method(m, "max", Math::max, 2);
        self.method(m, "min", Math::min, 2);
        self.direct(Math::max, j8framework::FastCall::A2(Math::max2), j8framework::FastOut::Number);
        self.direct(Math::min, j8framework::FastCall::A2(Math::min2), j8framework::FastOut::Number);
        self.method(m, "hypot", Math::hypot, 2);
        for (name, v) in [("PI", core::f64::consts::PI), ("E", core::f64::consts::E), ("LN2", core::f64::consts::LN_2), ("LN10", core::f64::consts::LN_10), ("LOG2E", core::f64::consts::LOG2_E), ("LOG10E", core::f64::consts::LOG10_E), ("SQRT2", core::f64::consts::SQRT_2), ("SQRT1_2", core::f64::consts::FRAC_1_SQRT_2)] {
            self.set(m, name, v.into());
        }
        self.set(self.realm.global, "Math", m.into());
    }
}
