use j8bytecode::{Decoded, WasmBytecode};

use super::{Float, Slot, WasmStore, WasmTrap};

pub trait Lane: Copy + Default {
    const BYTES: usize;
    fn read(b: &[u8]) -> Self;
    fn write(self, b: &mut [u8]);

    fn split<const N: usize>(v: u128) -> [Self; N] {
        let bytes = v.to_le_bytes();
        let mut out = [Self::default(); N];
        for (i, x) in out.iter_mut().enumerate() {
            *x = Self::read(&bytes[i * Self::BYTES..(i + 1) * Self::BYTES]);
        }
        out
    }

    fn join<const N: usize>(a: [Self; N]) -> u128 {
        let mut bytes = [0u8; 16];
        for (i, x) in a.iter().enumerate() {
            x.write(&mut bytes[i * Self::BYTES..(i + 1) * Self::BYTES]);
        }
        u128::from_le_bytes(bytes)
    }
}

macro_rules! lanes {
    ($($t:ty),*) => {$(
        impl Lane for $t {
            const BYTES: usize = core::mem::size_of::<$t>();
            fn read(b: &[u8]) -> Self {
                let mut a = [0u8; core::mem::size_of::<$t>()];
                a.copy_from_slice(b);
                <$t>::from_le_bytes(a)
            }
            fn write(self, b: &mut [u8]) {
                b.copy_from_slice(&self.to_le_bytes());
            }
        }
    )*};
}

lanes!(i8, u8, i16, u16, i32, u32, i64, u64, f32, f64);

struct Sat;

impl Sat {
    fn q15(x: i16, y: i16) -> i16 {
        let p = (i32::from(x) * i32::from(y) + 0x4000) >> 15;
        i16::try_from(p.clamp(i32::from(i16::MIN), i32::from(i16::MAX))).unwrap_or(i16::MAX)
    }

    fn i8(x: i16) -> i8 {
        i8::try_from(x.clamp(i16::from(i8::MIN), i16::from(i8::MAX))).unwrap_or(0)
    }

    fn u8(x: i16) -> u8 {
        u8::try_from(x.clamp(0, i16::from(u8::MAX))).unwrap_or(0)
    }

    fn i16(x: i32) -> i16 {
        i16::try_from(x.clamp(i32::from(i16::MIN), i32::from(i16::MAX))).unwrap_or(0)
    }

    fn u16(x: i32) -> u16 {
        u16::try_from(x.clamp(0, i32::from(u16::MAX))).unwrap_or(0)
    }

    fn quiet32(x: f32, f: fn(f32) -> f32) -> f32 {
        if x.is_nan() { x + x } else { f(x) }
    }

    fn quiet64(x: f64, f: fn(f64) -> f64) -> f64 {
        if x.is_nan() { x + x } else { f(x) }
    }
}

pub struct Vector<'s> {
    pub store: &'s mut WasmStore,
    pub fp: usize,
    pub instance: u32,
}

impl Vector<'_> {
    #[expect(clippy::too_many_lines, clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap, clippy::cast_precision_loss, clippy::float_cmp, clippy::cast_lossless, reason = "one arm per SIMD bytecode, lane-wise with WebAssembly's wrapping semantics")]
    pub fn run(self, d: &Decoded<WasmBytecode>, bits: &[u64]) -> Result<(), WasmTrap> {
        let (fp, instance) = (self.fp, self.instance);
        let o = d.operands;
        let at = |i: usize| fp + o[i] as usize;
        macro_rules! v {
            ($i:expr) => {
                self.store.vstack[at($i)]
            };
        }
        macro_rules! s {
            ($i:expr) => {
                self.store.stack[at($i)]
            };
        }
        macro_rules! map1 {
            ($t:ty, $n:literal, |$x:ident| $e:expr) => {{
                let a: [$t; $n] = Lane::split(v!(1));
                v!(0) = Lane::join(a.map(|$x| $e));
            }};
        }
        macro_rules! map2 {
            ($t:ty, $n:literal, |$x:ident, $y:ident| $e:expr) => {{
                let a: [$t; $n] = Lane::split(v!(1));
                let b: [$t; $n] = Lane::split(v!(2));
                let mut r = [<$t>::default(); $n];
                for i in 0..$n {
                    let ($x, $y) = (a[i], b[i]);
                    r[i] = $e;
                }
                v!(0) = Lane::join(r);
            }};
        }
        macro_rules! cmp {
            ($t:ty, $i:ty, $n:literal, |$x:ident, $y:ident| $e:expr) => {{
                let a: [$t; $n] = Lane::split(v!(1));
                let b: [$t; $n] = Lane::split(v!(2));
                let mut r = [<$i>::default(); $n];
                for i in 0..$n {
                    let ($x, $y) = (a[i], b[i]);
                    r[i] = if $e { !0 } else { 0 };
                }
                v!(0) = Lane::join(r);
            }};
        }
        macro_rules! shift {
            ($t:ty, $n:literal, $bits:literal, |$x:ident, $k:ident| $e:expr) => {{
                let $k = Slot(s!(2)).u32() % $bits;
                map1!($t, $n, |$x| $e);
            }};
        }
        macro_rules! narrow {
            ($from:ty, $to:ty, $n:literal, $f:expr) => {{
                let a: [$from; $n] = Lane::split(v!(1));
                let b: [$from; $n] = Lane::split(v!(2));
                let mut r = [<$to>::default(); $n * 2];
                for i in 0..$n {
                    r[i] = $f(a[i]);
                    r[i + $n] = $f(b[i]);
                }
                v!(0) = Lane::join(r);
            }};
        }
        macro_rules! widen {
            ($from:ty, $to:ty, $n:literal, $start:expr) => {{
                let a: [$from; { $n * 2 }] = Lane::split(v!(1));
                let mut r = [<$to>::default(); $n];
                for i in 0..$n {
                    r[i] = <$to>::from(a[$start + i]);
                }
                v!(0) = Lane::join(r);
            }};
        }
        macro_rules! extmul {
            ($from:ty, $to:ty, $n:literal, $start:expr) => {{
                let a: [$from; { $n * 2 }] = Lane::split(v!(1));
                let b: [$from; { $n * 2 }] = Lane::split(v!(2));
                let mut r = [<$to>::default(); $n];
                for i in 0..$n {
                    r[i] = <$to>::from(a[$start + i]).wrapping_mul(<$to>::from(b[$start + i]));
                }
                v!(0) = Lane::join(r);
            }};
        }
        macro_rules! pairwise {
            ($from:ty, $to:ty, $n:literal) => {{
                let a: [$from; { $n * 2 }] = Lane::split(v!(1));
                let mut r = [<$to>::default(); $n];
                for i in 0..$n {
                    r[i] = <$to>::from(a[2 * i]) + <$to>::from(a[2 * i + 1]);
                }
                v!(0) = Lane::join(r);
            }};
        }
        macro_rules! all_true {
            ($t:ty, $n:literal) => {{
                let a: [$t; $n] = Lane::split(v!(1));
                s!(0) = u64::from(a.iter().all(|&x| x != 0));
            }};
        }
        macro_rules! bitmask {
            ($t:ty, $n:literal) => {{
                let a: [$t; $n] = Lane::split(v!(1));
                s!(0) = a.iter().enumerate().fold(0u64, |m, (i, &x)| m | (u64::from(x < 0) << i));
            }};
        }
        macro_rules! splat {
            ($t:ty, $n:literal, $x:expr) => {{
                v!(0) = Lane::join([$x as $t; $n]);
            }};
        }
        macro_rules! extract {
            ($t:ty, $n:literal, |$x:ident| $e:expr) => {{
                let a: [$t; $n] = Lane::split(v!(1));
                let $x = a[o[2] as usize % $n];
                s!(0) = $e;
            }};
        }
        macro_rules! replace {
            ($t:ty, $n:literal, $x:expr) => {{
                let mut a: [$t; $n] = Lane::split(v!(1));
                a[o[3] as usize % $n] = $x;
                v!(0) = Lane::join(a);
            }};
        }
        macro_rules! trunc_sat {
            ($from:ty, $nf:literal, $to:ty) => {{
                let a: [$from; $nf] = Lane::split(v!(1));
                let mut r = [<$to>::default(); 4];
                for i in 0..$nf {
                    r[i] = a[i] as $to;
                }
                v!(0) = Lane::join(r);
            }};
        }
        macro_rules! load {
            ($n:literal) => {{
                let m = self.store.mem(instance, o[2]);
                let a = self.store.address(m, s!(1), o[3], $n)?;
                let mut b = [0u8; $n];
                b.copy_from_slice(&self.store.memories[m].bytes[a..a + $n]);
                b
            }};
        }
        macro_rules! load_extend {
            ($from:ty, $to:ty, $n:literal) => {{
                let b = load!(8);
                let a: [$from; $n] = core::array::from_fn(|i| <$from>::read(&b[i * <$from>::BYTES..(i + 1) * <$from>::BYTES]));
                v!(0) = Lane::join(a.map(<$to>::from));
            }};
        }
        macro_rules! lane_load {
            ($t:ty, $n:literal) => {{
                let m = self.store.mem(instance, o[3]);
                let a = self.store.address(m, s!(1), o[4], <$t>::BYTES)?;
                let x = <$t>::read(&self.store.memories[m].bytes[a..a + <$t>::BYTES]);
                let mut l: [$t; $n] = Lane::split(v!(2));
                l[o[5] as usize % $n] = x;
                v!(0) = Lane::join(l);
            }};
        }
        macro_rules! lane_store {
            ($t:ty, $n:literal) => {{
                let m = self.store.mem(instance, o[2]);
                let a = self.store.address(m, s!(0), o[3], <$t>::BYTES)?;
                let l: [$t; $n] = Lane::split(v!(1));
                l[o[4] as usize % $n].write(&mut self.store.memories[m].bytes[a..a + <$t>::BYTES]);
            }};
        }
        macro_rules! fma {
            ($t:ty, $n:literal, $neg:literal) => {{
                let a: [$t; $n] = Lane::split(v!(1));
                let b: [$t; $n] = Lane::split(v!(2));
                let c: [$t; $n] = Lane::split(v!(3));
                v!(0) = Lane::join(core::array::from_fn::<$t, $n, _>(|i| (if $neg { -a[i] } else { a[i] }).mul_add(b[i], c[i])));
            }};
        }
        let swizzle = |a: u128, s: u128| -> u128 {
            let (a, s): ([u8; 16], [u8; 16]) = (Lane::split(a), Lane::split(s));
            Lane::join(s.map(|i| if i < 16 { a[i as usize] } else { 0 }))
        };
        let pick = |a: u128, b: u128, c: u128| (a & c) | (b & !c);
        match d.bytecode {
            WasmBytecode::S128Const => v!(0) = u128::from(bits[o[1] as usize]) | (u128::from(bits[o[1] as usize + 1]) << 64),
            WasmBytecode::I8x16Shuffle => {
                let (a, b): ([u8; 16], [u8; 16]) = (Lane::split(v!(1)), Lane::split(v!(2)));
                let lanes: [u8; 16] = Lane::split(u128::from(bits[o[3] as usize]) | (u128::from(bits[o[3] as usize + 1]) << 64));
                v!(0) = Lane::join(lanes.map(|i| if i < 16 { a[i as usize] } else { b[(i - 16) as usize] }));
            }
            WasmBytecode::I8x16Swizzle | WasmBytecode::I8x16RelaxedSwizzle => v!(0) = swizzle(v!(1), v!(2)),
            WasmBytecode::I8x16Splat => splat!(u8, 16, s!(1)),
            WasmBytecode::I16x8Splat => splat!(u16, 8, s!(1)),
            WasmBytecode::I32x4Splat | WasmBytecode::F32x4Splat => splat!(u32, 4, s!(1)),
            WasmBytecode::I64x2Splat | WasmBytecode::F64x2Splat => splat!(u64, 2, s!(1)),
            WasmBytecode::I8x16ExtractLaneS => extract!(i8, 16, |x| i64::from(x) as u32 as u64),
            WasmBytecode::I8x16ExtractLaneU => extract!(u8, 16, |x| u64::from(x)),
            WasmBytecode::I16x8ExtractLaneS => extract!(i16, 8, |x| i64::from(x) as u32 as u64),
            WasmBytecode::I16x8ExtractLaneU => extract!(u16, 8, |x| u64::from(x)),
            WasmBytecode::I32x4ExtractLane | WasmBytecode::F32x4ExtractLane => extract!(u32, 4, |x| u64::from(x)),
            WasmBytecode::I64x2ExtractLane | WasmBytecode::F64x2ExtractLane => extract!(u64, 2, |x| x),
            WasmBytecode::I8x16ReplaceLane => replace!(u8, 16, s!(2) as u8),
            WasmBytecode::I16x8ReplaceLane => replace!(u16, 8, s!(2) as u16),
            WasmBytecode::I32x4ReplaceLane | WasmBytecode::F32x4ReplaceLane => replace!(u32, 4, s!(2) as u32),
            WasmBytecode::I64x2ReplaceLane | WasmBytecode::F64x2ReplaceLane => replace!(u64, 2, s!(2)),
            WasmBytecode::I8x16Eq => cmp!(u8, i8, 16, |x, y| x == y),
            WasmBytecode::I8x16Ne => cmp!(u8, i8, 16, |x, y| x != y),
            WasmBytecode::I8x16LtS => cmp!(i8, i8, 16, |x, y| x < y),
            WasmBytecode::I8x16LtU => cmp!(u8, i8, 16, |x, y| x < y),
            WasmBytecode::I8x16GtS => cmp!(i8, i8, 16, |x, y| x > y),
            WasmBytecode::I8x16GtU => cmp!(u8, i8, 16, |x, y| x > y),
            WasmBytecode::I8x16LeS => cmp!(i8, i8, 16, |x, y| x <= y),
            WasmBytecode::I8x16LeU => cmp!(u8, i8, 16, |x, y| x <= y),
            WasmBytecode::I8x16GeS => cmp!(i8, i8, 16, |x, y| x >= y),
            WasmBytecode::I8x16GeU => cmp!(u8, i8, 16, |x, y| x >= y),
            WasmBytecode::I16x8Eq => cmp!(u16, i16, 8, |x, y| x == y),
            WasmBytecode::I16x8Ne => cmp!(u16, i16, 8, |x, y| x != y),
            WasmBytecode::I16x8LtS => cmp!(i16, i16, 8, |x, y| x < y),
            WasmBytecode::I16x8LtU => cmp!(u16, i16, 8, |x, y| x < y),
            WasmBytecode::I16x8GtS => cmp!(i16, i16, 8, |x, y| x > y),
            WasmBytecode::I16x8GtU => cmp!(u16, i16, 8, |x, y| x > y),
            WasmBytecode::I16x8LeS => cmp!(i16, i16, 8, |x, y| x <= y),
            WasmBytecode::I16x8LeU => cmp!(u16, i16, 8, |x, y| x <= y),
            WasmBytecode::I16x8GeS => cmp!(i16, i16, 8, |x, y| x >= y),
            WasmBytecode::I16x8GeU => cmp!(u16, i16, 8, |x, y| x >= y),
            WasmBytecode::I32x4Eq => cmp!(u32, i32, 4, |x, y| x == y),
            WasmBytecode::I32x4Ne => cmp!(u32, i32, 4, |x, y| x != y),
            WasmBytecode::I32x4LtS => cmp!(i32, i32, 4, |x, y| x < y),
            WasmBytecode::I32x4LtU => cmp!(u32, i32, 4, |x, y| x < y),
            WasmBytecode::I32x4GtS => cmp!(i32, i32, 4, |x, y| x > y),
            WasmBytecode::I32x4GtU => cmp!(u32, i32, 4, |x, y| x > y),
            WasmBytecode::I32x4LeS => cmp!(i32, i32, 4, |x, y| x <= y),
            WasmBytecode::I32x4LeU => cmp!(u32, i32, 4, |x, y| x <= y),
            WasmBytecode::I32x4GeS => cmp!(i32, i32, 4, |x, y| x >= y),
            WasmBytecode::I32x4GeU => cmp!(u32, i32, 4, |x, y| x >= y),
            WasmBytecode::I64x2Eq => cmp!(u64, i64, 2, |x, y| x == y),
            WasmBytecode::I64x2Ne => cmp!(u64, i64, 2, |x, y| x != y),
            WasmBytecode::I64x2LtS => cmp!(i64, i64, 2, |x, y| x < y),
            WasmBytecode::I64x2GtS => cmp!(i64, i64, 2, |x, y| x > y),
            WasmBytecode::I64x2LeS => cmp!(i64, i64, 2, |x, y| x <= y),
            WasmBytecode::I64x2GeS => cmp!(i64, i64, 2, |x, y| x >= y),
            WasmBytecode::F32x4Eq => cmp!(f32, i32, 4, |x, y| x == y),
            WasmBytecode::F32x4Ne => cmp!(f32, i32, 4, |x, y| x != y),
            WasmBytecode::F32x4Lt => cmp!(f32, i32, 4, |x, y| x < y),
            WasmBytecode::F32x4Gt => cmp!(f32, i32, 4, |x, y| x > y),
            WasmBytecode::F32x4Le => cmp!(f32, i32, 4, |x, y| x <= y),
            WasmBytecode::F32x4Ge => cmp!(f32, i32, 4, |x, y| x >= y),
            WasmBytecode::F64x2Eq => cmp!(f64, i64, 2, |x, y| x == y),
            WasmBytecode::F64x2Ne => cmp!(f64, i64, 2, |x, y| x != y),
            WasmBytecode::F64x2Lt => cmp!(f64, i64, 2, |x, y| x < y),
            WasmBytecode::F64x2Gt => cmp!(f64, i64, 2, |x, y| x > y),
            WasmBytecode::F64x2Le => cmp!(f64, i64, 2, |x, y| x <= y),
            WasmBytecode::F64x2Ge => cmp!(f64, i64, 2, |x, y| x >= y),
            WasmBytecode::S128Not => v!(0) = !v!(1),
            WasmBytecode::S128And => v!(0) = v!(1) & v!(2),
            WasmBytecode::S128AndNot => v!(0) = v!(1) & !v!(2),
            WasmBytecode::S128Or => v!(0) = v!(1) | v!(2),
            WasmBytecode::S128Xor => v!(0) = v!(1) ^ v!(2),
            WasmBytecode::S128Select | WasmBytecode::I8x16RelaxedLaneSelect | WasmBytecode::I16x8RelaxedLaneSelect | WasmBytecode::I32x4RelaxedLaneSelect | WasmBytecode::I64x2RelaxedLaneSelect => v!(0) = pick(v!(1), v!(2), v!(3)),
            WasmBytecode::V128AnyTrue => s!(0) = u64::from(v!(1) != 0),
            WasmBytecode::I8x16Abs => map1!(i8, 16, |x| x.wrapping_abs()),
            WasmBytecode::I8x16Neg => map1!(i8, 16, |x| x.wrapping_neg()),
            WasmBytecode::I8x16Popcnt => map1!(u8, 16, |x| x.count_ones() as u8),
            WasmBytecode::I8x16AllTrue => all_true!(u8, 16),
            WasmBytecode::I8x16BitMask => bitmask!(i8, 16),
            WasmBytecode::I8x16SConvertI16x8 => narrow!(i16, i8, 8, Sat::i8),
            WasmBytecode::I8x16UConvertI16x8 => narrow!(i16, u8, 8, Sat::u8),
            WasmBytecode::I8x16Shl => shift!(u8, 16, 8, |x, k| x.wrapping_shl(k)),
            WasmBytecode::I8x16ShrS => shift!(i8, 16, 8, |x, k| x.wrapping_shr(k)),
            WasmBytecode::I8x16ShrU => shift!(u8, 16, 8, |x, k| x.wrapping_shr(k)),
            WasmBytecode::I8x16Add => map2!(u8, 16, |x, y| x.wrapping_add(y)),
            WasmBytecode::I8x16AddSatS => map2!(i8, 16, |x, y| x.saturating_add(y)),
            WasmBytecode::I8x16AddSatU => map2!(u8, 16, |x, y| x.saturating_add(y)),
            WasmBytecode::I8x16Sub => map2!(u8, 16, |x, y| x.wrapping_sub(y)),
            WasmBytecode::I8x16SubSatS => map2!(i8, 16, |x, y| x.saturating_sub(y)),
            WasmBytecode::I8x16SubSatU => map2!(u8, 16, |x, y| x.saturating_sub(y)),
            WasmBytecode::I8x16MinS => map2!(i8, 16, |x, y| x.min(y)),
            WasmBytecode::I8x16MinU => map2!(u8, 16, |x, y| x.min(y)),
            WasmBytecode::I8x16MaxS => map2!(i8, 16, |x, y| x.max(y)),
            WasmBytecode::I8x16MaxU => map2!(u8, 16, |x, y| x.max(y)),
            WasmBytecode::I8x16RoundingAverageU => map2!(u8, 16, |x, y| (u16::from(x) + u16::from(y)).div_ceil(2) as u8),
            WasmBytecode::I16x8ExtAddPairwiseI8x16S => pairwise!(i8, i16, 8),
            WasmBytecode::I16x8ExtAddPairwiseI8x16U => pairwise!(u8, u16, 8),
            WasmBytecode::I32x4ExtAddPairwiseI16x8S => pairwise!(i16, i32, 4),
            WasmBytecode::I32x4ExtAddPairwiseI16x8U => pairwise!(u16, u32, 4),
            WasmBytecode::I16x8Abs => map1!(i16, 8, |x| x.wrapping_abs()),
            WasmBytecode::I16x8Neg => map1!(i16, 8, |x| x.wrapping_neg()),
            WasmBytecode::I16x8Q15MulRSatS | WasmBytecode::I16x8RelaxedQ15MulRS => map2!(i16, 8, |x, y| Sat::q15(x, y)),
            WasmBytecode::I16x8AllTrue => all_true!(u16, 8),
            WasmBytecode::I16x8BitMask => bitmask!(i16, 8),
            WasmBytecode::I16x8SConvertI32x4 => narrow!(i32, i16, 4, Sat::i16),
            WasmBytecode::I16x8UConvertI32x4 => narrow!(i32, u16, 4, Sat::u16),
            WasmBytecode::I16x8SConvertI8x16Low => widen!(i8, i16, 8, 0),
            WasmBytecode::I16x8SConvertI8x16High => widen!(i8, i16, 8, 8),
            WasmBytecode::I16x8UConvertI8x16Low => widen!(u8, u16, 8, 0),
            WasmBytecode::I16x8UConvertI8x16High => widen!(u8, u16, 8, 8),
            WasmBytecode::I16x8Shl => shift!(u16, 8, 16, |x, k| x.wrapping_shl(k)),
            WasmBytecode::I16x8ShrS => shift!(i16, 8, 16, |x, k| x.wrapping_shr(k)),
            WasmBytecode::I16x8ShrU => shift!(u16, 8, 16, |x, k| x.wrapping_shr(k)),
            WasmBytecode::I16x8Add => map2!(u16, 8, |x, y| x.wrapping_add(y)),
            WasmBytecode::I16x8AddSatS => map2!(i16, 8, |x, y| x.saturating_add(y)),
            WasmBytecode::I16x8AddSatU => map2!(u16, 8, |x, y| x.saturating_add(y)),
            WasmBytecode::I16x8Sub => map2!(u16, 8, |x, y| x.wrapping_sub(y)),
            WasmBytecode::I16x8SubSatS => map2!(i16, 8, |x, y| x.saturating_sub(y)),
            WasmBytecode::I16x8SubSatU => map2!(u16, 8, |x, y| x.saturating_sub(y)),
            WasmBytecode::I16x8Mul => map2!(u16, 8, |x, y| x.wrapping_mul(y)),
            WasmBytecode::I16x8MinS => map2!(i16, 8, |x, y| x.min(y)),
            WasmBytecode::I16x8MinU => map2!(u16, 8, |x, y| x.min(y)),
            WasmBytecode::I16x8MaxS => map2!(i16, 8, |x, y| x.max(y)),
            WasmBytecode::I16x8MaxU => map2!(u16, 8, |x, y| x.max(y)),
            WasmBytecode::I16x8RoundingAverageU => map2!(u16, 8, |x, y| (u32::from(x) + u32::from(y)).div_ceil(2) as u16),
            WasmBytecode::I16x8ExtMulLowI8x16S => extmul!(i8, i16, 8, 0),
            WasmBytecode::I16x8ExtMulHighI8x16S => extmul!(i8, i16, 8, 8),
            WasmBytecode::I16x8ExtMulLowI8x16U => extmul!(u8, u16, 8, 0),
            WasmBytecode::I16x8ExtMulHighI8x16U => extmul!(u8, u16, 8, 8),
            WasmBytecode::I32x4Abs => map1!(i32, 4, |x| x.wrapping_abs()),
            WasmBytecode::I32x4Neg => map1!(i32, 4, |x| x.wrapping_neg()),
            WasmBytecode::I32x4AllTrue => all_true!(u32, 4),
            WasmBytecode::I32x4BitMask => bitmask!(i32, 4),
            WasmBytecode::I32x4SConvertI16x8Low => widen!(i16, i32, 4, 0),
            WasmBytecode::I32x4SConvertI16x8High => widen!(i16, i32, 4, 4),
            WasmBytecode::I32x4UConvertI16x8Low => widen!(u16, u32, 4, 0),
            WasmBytecode::I32x4UConvertI16x8High => widen!(u16, u32, 4, 4),
            WasmBytecode::I32x4Shl => shift!(u32, 4, 32, |x, k| x.wrapping_shl(k)),
            WasmBytecode::I32x4ShrS => shift!(i32, 4, 32, |x, k| x.wrapping_shr(k)),
            WasmBytecode::I32x4ShrU => shift!(u32, 4, 32, |x, k| x.wrapping_shr(k)),
            WasmBytecode::I32x4Add => map2!(u32, 4, |x, y| x.wrapping_add(y)),
            WasmBytecode::I32x4Sub => map2!(u32, 4, |x, y| x.wrapping_sub(y)),
            WasmBytecode::I32x4Mul => map2!(u32, 4, |x, y| x.wrapping_mul(y)),
            WasmBytecode::I32x4MinS => map2!(i32, 4, |x, y| x.min(y)),
            WasmBytecode::I32x4MinU => map2!(u32, 4, |x, y| x.min(y)),
            WasmBytecode::I32x4MaxS => map2!(i32, 4, |x, y| x.max(y)),
            WasmBytecode::I32x4MaxU => map2!(u32, 4, |x, y| x.max(y)),
            WasmBytecode::I32x4DotI16x8S => {
                let (a, b): ([i16; 8], [i16; 8]) = (Lane::split(v!(1)), Lane::split(v!(2)));
                v!(0) = Lane::join(core::array::from_fn::<i32, 4, _>(|i| (i32::from(a[2 * i]) * i32::from(b[2 * i])).wrapping_add(i32::from(a[2 * i + 1]) * i32::from(b[2 * i + 1]))));
            }
            WasmBytecode::I16x8DotI8x16I7x16S => {
                let (a, b): ([i8; 16], [i8; 16]) = (Lane::split(v!(1)), Lane::split(v!(2)));
                v!(0) = Lane::join(core::array::from_fn::<i16, 8, _>(|i| (i16::from(a[2 * i]) * i16::from(b[2 * i])).wrapping_add(i16::from(a[2 * i + 1]) * i16::from(b[2 * i + 1]))));
            }
            WasmBytecode::I32x4DotI8x16I7x16AddS => {
                let (a, b, c): ([i8; 16], [i8; 16], [i32; 4]) = (Lane::split(v!(1)), Lane::split(v!(2)), Lane::split(v!(3)));
                v!(0) = Lane::join(core::array::from_fn::<i32, 4, _>(|i| (0..4).fold(c[i], |s, j| s.wrapping_add(i32::from(a[4 * i + j]) * i32::from(b[4 * i + j])))));
            }
            WasmBytecode::I32x4ExtMulLowI16x8S => extmul!(i16, i32, 4, 0),
            WasmBytecode::I32x4ExtMulHighI16x8S => extmul!(i16, i32, 4, 4),
            WasmBytecode::I32x4ExtMulLowI16x8U => extmul!(u16, u32, 4, 0),
            WasmBytecode::I32x4ExtMulHighI16x8U => extmul!(u16, u32, 4, 4),
            WasmBytecode::I64x2Abs => map1!(i64, 2, |x| x.wrapping_abs()),
            WasmBytecode::I64x2Neg => map1!(i64, 2, |x| x.wrapping_neg()),
            WasmBytecode::I64x2AllTrue => all_true!(u64, 2),
            WasmBytecode::I64x2BitMask => bitmask!(i64, 2),
            WasmBytecode::I64x2SConvertI32x4Low => widen!(i32, i64, 2, 0),
            WasmBytecode::I64x2SConvertI32x4High => widen!(i32, i64, 2, 2),
            WasmBytecode::I64x2UConvertI32x4Low => widen!(u32, u64, 2, 0),
            WasmBytecode::I64x2UConvertI32x4High => widen!(u32, u64, 2, 2),
            WasmBytecode::I64x2Shl => shift!(u64, 2, 64, |x, k| x.wrapping_shl(k)),
            WasmBytecode::I64x2ShrS => shift!(i64, 2, 64, |x, k| x.wrapping_shr(k)),
            WasmBytecode::I64x2ShrU => shift!(u64, 2, 64, |x, k| x.wrapping_shr(k)),
            WasmBytecode::I64x2Add => map2!(u64, 2, |x, y| x.wrapping_add(y)),
            WasmBytecode::I64x2Sub => map2!(u64, 2, |x, y| x.wrapping_sub(y)),
            WasmBytecode::I64x2Mul => map2!(u64, 2, |x, y| x.wrapping_mul(y)),
            WasmBytecode::I64x2ExtMulLowI32x4S => extmul!(i32, i64, 2, 0),
            WasmBytecode::I64x2ExtMulHighI32x4S => extmul!(i32, i64, 2, 2),
            WasmBytecode::I64x2ExtMulLowI32x4U => extmul!(u32, u64, 2, 0),
            WasmBytecode::I64x2ExtMulHighI32x4U => extmul!(u32, u64, 2, 2),
            WasmBytecode::F32x4Abs => map1!(u32, 4, |x| x & 0x7fff_ffff),
            WasmBytecode::F32x4Neg => map1!(u32, 4, |x| x ^ 0x8000_0000),
            WasmBytecode::F32x4Sqrt => map1!(f32, 4, |x| x.sqrt()),
            WasmBytecode::F32x4Ceil => map1!(f32, 4, |x| Sat::quiet32(x, f32::ceil)),
            WasmBytecode::F32x4Floor => map1!(f32, 4, |x| Sat::quiet32(x, f32::floor)),
            WasmBytecode::F32x4Trunc => map1!(f32, 4, |x| Sat::quiet32(x, f32::trunc)),
            WasmBytecode::F32x4NearestInt => map1!(f32, 4, |x| Sat::quiet32(x, f32::round_ties_even)),
            WasmBytecode::F32x4Add => map2!(f32, 4, |x, y| x + y),
            WasmBytecode::F32x4Sub => map2!(f32, 4, |x, y| x - y),
            WasmBytecode::F32x4Mul => map2!(f32, 4, |x, y| x * y),
            WasmBytecode::F32x4Div => map2!(f32, 4, |x, y| x / y),
            WasmBytecode::F32x4Min | WasmBytecode::F32x4RelaxedMin => map2!(f32, 4, |x, y| Float::min32(x, y)),
            WasmBytecode::F32x4Max | WasmBytecode::F32x4RelaxedMax => map2!(f32, 4, |x, y| Float::max32(x, y)),
            WasmBytecode::F32x4Pmin => map2!(f32, 4, |x, y| if y < x { y } else { x }),
            WasmBytecode::F32x4Pmax => map2!(f32, 4, |x, y| if x < y { y } else { x }),
            WasmBytecode::F64x2Abs => map1!(u64, 2, |x| x & 0x7fff_ffff_ffff_ffff),
            WasmBytecode::F64x2Neg => map1!(u64, 2, |x| x ^ 0x8000_0000_0000_0000),
            WasmBytecode::F64x2Sqrt => map1!(f64, 2, |x| x.sqrt()),
            WasmBytecode::F64x2Ceil => map1!(f64, 2, |x| Sat::quiet64(x, f64::ceil)),
            WasmBytecode::F64x2Floor => map1!(f64, 2, |x| Sat::quiet64(x, f64::floor)),
            WasmBytecode::F64x2Trunc => map1!(f64, 2, |x| Sat::quiet64(x, f64::trunc)),
            WasmBytecode::F64x2NearestInt => map1!(f64, 2, |x| Sat::quiet64(x, f64::round_ties_even)),
            WasmBytecode::F64x2Add => map2!(f64, 2, |x, y| x + y),
            WasmBytecode::F64x2Sub => map2!(f64, 2, |x, y| x - y),
            WasmBytecode::F64x2Mul => map2!(f64, 2, |x, y| x * y),
            WasmBytecode::F64x2Div => map2!(f64, 2, |x, y| x / y),
            WasmBytecode::F64x2Min | WasmBytecode::F64x2RelaxedMin => map2!(f64, 2, |x, y| Float::min64(x, y)),
            WasmBytecode::F64x2Max | WasmBytecode::F64x2RelaxedMax => map2!(f64, 2, |x, y| Float::max64(x, y)),
            WasmBytecode::F64x2Pmin => map2!(f64, 2, |x, y| if y < x { y } else { x }),
            WasmBytecode::F64x2Pmax => map2!(f64, 2, |x, y| if x < y { y } else { x }),
            WasmBytecode::I32x4SConvertF32x4 | WasmBytecode::I32x4RelaxedTruncF32x4S => trunc_sat!(f32, 4, i32),
            WasmBytecode::I32x4UConvertF32x4 | WasmBytecode::I32x4RelaxedTruncF32x4U => trunc_sat!(f32, 4, u32),
            WasmBytecode::I32x4TruncSatF64x2SZero | WasmBytecode::I32x4RelaxedTruncF64x2SZero => trunc_sat!(f64, 2, i32),
            WasmBytecode::I32x4TruncSatF64x2UZero | WasmBytecode::I32x4RelaxedTruncF64x2UZero => trunc_sat!(f64, 2, u32),
            WasmBytecode::F32x4SConvertI32x4 => map1!(i32, 4, |x| (x as f32).to_bits() as i32),
            WasmBytecode::F32x4UConvertI32x4 => map1!(u32, 4, |x| (x as f32).to_bits()),
            WasmBytecode::F64x2ConvertLowI32x4S => {
                let a: [i32; 4] = Lane::split(v!(1));
                v!(0) = Lane::join([f64::from(a[0]), f64::from(a[1])]);
            }
            WasmBytecode::F64x2ConvertLowI32x4U => {
                let a: [u32; 4] = Lane::split(v!(1));
                v!(0) = Lane::join([f64::from(a[0]), f64::from(a[1])]);
            }
            WasmBytecode::F32x4DemoteF64x2Zero => {
                let a: [f64; 2] = Lane::split(v!(1));
                v!(0) = Lane::join([a[0] as f32, a[1] as f32, 0.0, 0.0]);
            }
            WasmBytecode::F64x2PromoteLowF32x4 => {
                let a: [f32; 4] = Lane::split(v!(1));
                v!(0) = Lane::join([f64::from(a[0]), f64::from(a[1])]);
            }
            WasmBytecode::F32x4Qfma => fma!(f32, 4, false),
            WasmBytecode::F32x4Qfms => fma!(f32, 4, true),
            WasmBytecode::F64x2Qfma => fma!(f64, 2, false),
            WasmBytecode::F64x2Qfms => fma!(f64, 2, true),
            WasmBytecode::S128LoadMem => v!(0) = u128::from_le_bytes(load!(16)),
            WasmBytecode::S128Load8x8S => load_extend!(i8, i16, 8),
            WasmBytecode::S128Load8x8U => load_extend!(u8, u16, 8),
            WasmBytecode::S128Load16x4S => load_extend!(i16, i32, 4),
            WasmBytecode::S128Load16x4U => load_extend!(u16, u32, 4),
            WasmBytecode::S128Load32x2S => load_extend!(i32, i64, 2),
            WasmBytecode::S128Load32x2U => load_extend!(u32, u64, 2),
            WasmBytecode::S128Load8Splat => splat!(u8, 16, load!(1)[0]),
            WasmBytecode::S128Load16Splat => splat!(u16, 8, u16::from_le_bytes(load!(2))),
            WasmBytecode::S128Load32Splat => splat!(u32, 4, u32::from_le_bytes(load!(4))),
            WasmBytecode::S128Load64Splat => splat!(u64, 2, u64::from_le_bytes(load!(8))),
            WasmBytecode::S128Load32Zero => v!(0) = u128::from(u32::from_le_bytes(load!(4))),
            WasmBytecode::S128Load64Zero => v!(0) = u128::from(u64::from_le_bytes(load!(8))),
            WasmBytecode::S128Load8Lane => lane_load!(u8, 16),
            WasmBytecode::S128Load16Lane => lane_load!(u16, 8),
            WasmBytecode::S128Load32Lane => lane_load!(u32, 4),
            WasmBytecode::S128Load64Lane => lane_load!(u64, 2),
            WasmBytecode::S128StoreMem => {
                let m = self.store.mem(instance, o[2]);
                let a = self.store.address(m, s!(0), o[3], 16)?;
                let b = v!(1).to_le_bytes();
                self.store.memories[m].bytes[a..a + 16].copy_from_slice(&b);
            }
            WasmBytecode::S128Store8Lane => lane_store!(u8, 16),
            WasmBytecode::S128Store16Lane => lane_store!(u16, 8),
            WasmBytecode::S128Store32Lane => lane_store!(u32, 4),
            WasmBytecode::S128Store64Lane => lane_store!(u64, 2),
            _ => return Err(WasmTrap::Unsupported),
        }
        Ok(())
    }
}
