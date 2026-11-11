use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;

use r8frame::{Agent, Error, ErrorKind, JsObject, ObjectData, ObjectKind, Property, Slot, Value};

use crate::{Args, Installer};

/// `Array` 与 `Array.prototype`：方法都按规范对任意类数组通用（经 `length` 与下标属性）。
struct Arrays;

/// 2^53 − 1：类数组长度的上限
const MAX_LENGTH: f64 = 9_007_199_254_740_991.0;
/// 2^32 − 1：真数组长度的上限
const MAX_ARRAY_LENGTH: f64 = 4_294_967_295.0;

/// 回调方法的结果怎么收。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Visit {
    ForEach,
    Map,
    Filter,
    Some,
    Every,
}

/// `find` 一族：方向与给值还是下标。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Find {
    Value,
    Index,
    LastValue,
    LastIndex,
}

impl Arrays {
    /// O = ToObject(this)，len = LengthOfArrayLike(O)。
    fn target<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>) -> Result<(JsObject<'gc>, f64), Error> {
        let o = a.to_object(this)?;
        let len = a.length_of_array_like(o.into())?;
        Ok((o, len))
    }

    /// ArrayCreate(len)：长度超过 2^32 − 1 抛 RangeError，元素全是空洞。
    fn create<'gc>(a: &mut Agent<'gc, '_>, len: f64, proto: Option<JsObject<'gc>>) -> Result<JsObject<'gc>, Error> {
        if len > MAX_ARRAY_LENGTH {
            return Err(a.throw(ErrorKind::Range, "Invalid array length"));
        }
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "已检查在 [0, 2^32 − 1] 里")]
        let n = len as u32;
        let arr = ObjectData { kind: ObjectKind::Array(Vec::new()), proto: Some(proto.unwrap_or(a.realm.array_proto)), ..ObjectData::default() }.alloc(a.mc);
        arr.borrow_mut(a.mc).set_array_length(n);
        Ok(arr)
    }

    /// ArraySpeciesCreate(original, len)。
    fn species_create<'gc>(a: &mut Agent<'gc, '_>, original: JsObject<'gc>, len: f64) -> Result<JsObject<'gc>, Error> {
        if !a.is_array(original.into())? {
            return Self::create(a, len, None);
        }
        let mut c = a.get_named(original.into(), "constructor")?;
        if matches!(c, Value::Object(_)) {
            c = a.get_symbol(c, a.realm.symbols.species, c)?;
            if c == Value::Null {
                c = Value::Undefined;
            }
        }
        if c == Value::Undefined {
            return Self::create(a, len, None);
        }
        if !a.is_constructor(c) {
            return Err(a.throw(ErrorKind::Type, "object.constructor[Symbol.species] is not a constructor"));
        }
        match a.construct(c, &[len.into()], c)? {
            Value::Object(o) => Ok(o),
            _ => Err(a.throw(ErrorKind::Type, "species constructor returned a non-object")),
        }
    }

    /// `Set(O, "length", n, true)`。
    fn set_length<'gc>(a: &mut Agent<'gc, '_>, o: JsObject<'gc>, n: f64) -> Result<(), Error> {
        a.set_named(o.into(), "length", n.into())
    }

    /// 相对下标（负数从末尾数），夹到 `[0, len]`；`undefined` 给 `default`。
    fn relative<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>, len: f64, default: f64) -> Result<f64, Error> {
        if v == Value::Undefined {
            return Ok(default);
        }
        let x = a.to_integer_or_infinity(v)?;
        Ok(if x < 0.0 { (len + x).max(0.0) } else { x.min(len) })
    }

    fn callback<'gc>(a: &mut Agent<'gc, '_>, f: Value<'gc>) -> Result<(), Error> {
        if a.is_callable(f) {
            Ok(())
        } else {
            let t = a.describe(f);
            Err(a.throw(ErrorKind::Type, &alloc::format!("{t} is not a function")))
        }
    }

    /// `new Array(...)` / `Array(...)`。
    fn array<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let proto = a.proto_from_ctor(a.new_target, a.realm.array_proto)?;
        match args {
            [] => Ok(Self::create(a, 0.0, Some(proto))?.into()),
            [Value::Number(n)] => {
                let len = a.to_uint32(Value::Number(*n))?;
                if f64::from(len) != *n {
                    return Err(a.throw(ErrorKind::Range, "Invalid array length"));
                }
                Ok(Self::create(a, f64::from(len), Some(proto))?.into())
            }
            items => {
                let arr = a.array(items.to_vec());
                arr.borrow_mut(a.mc).proto = Some(proto);
                Ok(arr.into())
            }
        }
    }

    fn is_array<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(a.is_array(args.at(0))?.into())
    }

    /// `this` 能构造就 `new this(len?)`，否则 ArrayCreate。
    fn construct_this<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, len: Option<f64>) -> Result<JsObject<'gc>, Error> {
        if a.is_constructor(this) {
            let args: Vec<Value<'gc>> = len.map(Value::from).into_iter().collect();
            match a.construct(this, &args, this)? {
                Value::Object(o) => Ok(o),
                _ => Err(a.throw(ErrorKind::Type, "constructor returned a non-object")),
            }
        } else {
            Self::create(a, len.unwrap_or(0.0), None)
        }
    }

    /// `Array.of(...items)`。
    fn of<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        #[expect(clippy::cast_precision_loss, reason = "实参个数远小于 2^53")]
        let len = args.len() as f64;
        let arr = Self::construct_this(a, this, Some(len))?;
        for (k, &v) in (0..).zip(args) {
            a.create_index(arr, f64::from(k), v)?;
        }
        Self::set_length(a, arr, len)?;
        Ok(arr.into())
    }

    /// `Array.from(items, mapFn, thisArg)`。
    fn from<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (items, mapper, this_arg) = (args.at(0), args.at(1), args.at(2));
        let mapping = mapper != Value::Undefined;
        if mapping {
            Self::callback(a, mapper)?;
        }
        let using = if items.is_nullish() { Value::Undefined } else { a.get_symbol(items, a.realm.symbols.iterator, items)? };
        if !using.is_nullish() {
            if !a.is_callable(using) {
                return Err(a.throw(ErrorKind::Type, "Symbol.iterator is not a function"));
            }
            let arr = Self::construct_this(a, this, None)?;
            let iterator = a.call(using, items, &[])?;
            if !matches!(iterator, Value::Object(_)) {
                return Err(a.throw(ErrorKind::Type, "Result of the Symbol.iterator method is not an object"));
            }
            let next = a.get_named(iterator, "next")?;
            let it = r8frame::IterRecord { iterator, next };
            let mut k = 0.0;
            while let Some(v) = a.iterator_step(it)? {
                let r = (|| {
                    if k >= MAX_LENGTH {
                        return Err(a.throw(ErrorKind::Type, "Array.from: too many elements"));
                    }
                    let v = if mapping { a.call(mapper, this_arg, &[v, k.into()])? } else { v };
                    a.create_index(arr, k, v)
                })();
                if let Err(e) = r {
                    let _ = a.iterator_close(iterator);
                    return Err(e);
                }
                k += 1.0;
            }
            Self::set_length(a, arr, k)?;
            return Ok(arr.into());
        }
        let like = a.to_object(items)?;
        let len = a.length_of_array_like(like.into())?;
        let arr = Self::construct_this(a, this, Some(len))?;
        let mut k = 0.0;
        while k < len {
            let v = a.get_index(like.into(), k)?;
            let v = if mapping { a.call(mapper, this_arg, &[v, k.into()])? } else { v };
            a.create_index(arr, k, v)?;
            k += 1.0;
        }
        Self::set_length(a, arr, len)?;
        Ok(arr.into())
    }

    fn species<'gc>(_: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(this)
    }

    fn at<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        let rel = a.to_integer_or_infinity(args.at(0))?;
        let k = if rel >= 0.0 { rel } else { len + rel };
        if k < 0.0 || k >= len {
            return Ok(Value::Undefined);
        }
        a.get_index(o.into(), k)
    }

    fn push<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        // 快路：稠密、可扩展的真数组，直接追加
        if Self::no_indexed_protos(o) {
            let mut d = o.borrow_mut(a.mc);
            let fast = !d.non_extensible && !d.length_readonly && d.sparse_length == 0;
            if let (true, ObjectKind::Array(items)) = (fast, &mut d.kind) {
                items.extend_from_slice(args);
                #[expect(clippy::cast_precision_loss, reason = "数组长度不超过 2^32")]
                let n = items.len() as f64;
                return Ok(n.into());
            }
        }
        #[expect(clippy::cast_precision_loss, reason = "实参个数远小于 2^53")]
        let count = args.len() as f64;
        if len + count > MAX_LENGTH {
            return Err(a.throw(ErrorKind::Type, "Pushing elements past 2^53 - 1 length"));
        }
        let mut n = len;
        for &v in args {
            a.set_index(o.into(), n, v)?;
            n += 1.0;
        }
        Self::set_length(a, o, n)?;
        Ok(n.into())
    }

    /// 原型链上（数组本身之外）没有下标属性与访问器：追加 / 前移时不会碰到 setter。
    fn no_indexed_protos(o: JsObject<'_>) -> bool {
        let mut cur = o.borrow().proto;
        while let Some(p) = cur {
            let d = p.borrow();
            let indexed = match &d.kind {
                ObjectKind::Array(items) => !items.is_empty(),
                ObjectKind::Ordinary | ObjectKind::Error | ObjectKind::Arguments => false,
                _ => true,
            };
            if indexed || d.sparse_length > 0 || d.props.iter().any(|(k, _)| k.as_bytes().first().is_some_and(u8::is_ascii_digit)) {
                return false;
            }
            cur = d.proto;
        }
        true
    }

    fn pop<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        if len == 0.0 {
            Self::set_length(a, o, 0.0)?;
            return Ok(Value::Undefined);
        }
        let new_len = len - 1.0;
        let v = a.get_index(o.into(), new_len)?;
        a.delete_index(o, new_len)?;
        Self::set_length(a, o, new_len)?;
        Ok(v)
    }

    fn shift<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        if len == 0.0 {
            Self::set_length(a, o, 0.0)?;
            return Ok(Value::Undefined);
        }
        let first = a.get_index(o.into(), 0.0)?;
        // 快路：稠密真数组整体前移
        if Self::no_indexed_protos(o) {
            let mut d = o.borrow_mut(a.mc);
            let fast = !d.non_extensible && !d.length_readonly && d.sparse_length == 0;
            if let (true, ObjectKind::Array(items)) = (fast, &mut d.kind)
                && !items.is_empty()
            {
                items.remove(0);
                return Ok(first);
            }
        }
        Self::move_range(a, o, 1.0, 0.0, len - 1.0)?;
        a.delete_index(o, len - 1.0)?;
        Self::set_length(a, o, len - 1.0)?;
        Ok(first)
    }

    /// 把 `[from, from + count)` 挪到 `to` 起（空洞照搬成删除），按方向逐个来。
    fn move_range<'gc>(a: &mut Agent<'gc, '_>, o: JsObject<'gc>, from: f64, to: f64, count: f64) -> Result<(), Error> {
        let step = |a: &mut Agent<'gc, '_>, f: f64, t: f64| -> Result<(), Error> {
            if a.has_index(o, f)? {
                let v = a.get_index(o.into(), f)?;
                a.set_index(o.into(), t, v)
            } else {
                a.delete_index(o, t)
            }
        };
        if from > to {
            let mut k = 0.0;
            while k < count {
                step(a, from + k, to + k)?;
                k += 1.0;
            }
        } else {
            let mut k = count;
            while k > 0.0 {
                step(a, from + k - 1.0, to + k - 1.0)?;
                k -= 1.0;
            }
        }
        Ok(())
    }

    fn unshift<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        #[expect(clippy::cast_precision_loss, reason = "实参个数远小于 2^53")]
        let count = args.len() as f64;
        if count > 0.0 {
            if len + count > MAX_LENGTH {
                return Err(a.throw(ErrorKind::Type, "Unshifting elements past 2^53 - 1 length"));
            }
            Self::move_range(a, o, 0.0, count, len)?;
            for (j, &v) in (0..).zip(args) {
                a.set_index(o.into(), f64::from(j), v)?;
            }
        }
        Self::set_length(a, o, len + count)?;
        Ok((len + count).into())
    }

    fn slice<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        let mut k = Self::relative(a, args.at(0), len, 0.0)?;
        let end = Self::relative(a, args.at(1), len, len)?;
        let count = (end - k).max(0.0);
        let arr = Self::species_create(a, o, count)?;
        let mut n = 0.0;
        while k < end {
            if a.has_index(o, k)? {
                let v = a.get_index(o.into(), k)?;
                a.create_index(arr, n, v)?;
            }
            k += 1.0;
            n += 1.0;
        }
        Self::set_length(a, arr, n)?;
        Ok(arr.into())
    }

    fn splice<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        let start = Self::relative(a, args.at(0), len, 0.0)?;
        let items = args.get(2..).unwrap_or(&[]);
        #[expect(clippy::cast_precision_loss, reason = "实参个数远小于 2^53")]
        let item_count = items.len() as f64;
        let delete_count = match args.len() {
            0 => 0.0,
            1 => len - start,
            _ => a.to_integer_or_infinity(args[1])?.clamp(0.0, len - start),
        };
        if len + item_count - delete_count > MAX_LENGTH {
            return Err(a.throw(ErrorKind::Type, "Splice result exceeds 2^53 - 1 length"));
        }
        let removed = Self::species_create(a, o, delete_count)?;
        let mut k = 0.0;
        while k < delete_count {
            if a.has_index(o, start + k)? {
                let v = a.get_index(o.into(), start + k)?;
                a.create_index(removed, k, v)?;
            }
            k += 1.0;
        }
        Self::set_length(a, removed, delete_count)?;
        Self::shift_tail(a, o, len, start, delete_count, item_count)?;
        for (j, &v) in (0..).zip(items) {
            a.set_index(o.into(), start + f64::from(j), v)?;
        }
        Self::set_length(a, o, len - delete_count + item_count)?;
        Ok(removed.into())
    }

    /// splice 里挪动删除区之后的元素：删 `delete_count` 个、留出 `item_count` 个位置。
    fn shift_tail<'gc>(a: &mut Agent<'gc, '_>, o: JsObject<'gc>, len: f64, start: f64, delete_count: f64, item_count: f64) -> Result<(), Error> {
        if item_count < delete_count {
            Self::move_range(a, o, start + delete_count, start + item_count, len - start - delete_count)?;
            let mut k = len;
            while k > len - delete_count + item_count {
                a.delete_index(o, k - 1.0)?;
                k -= 1.0;
            }
        } else if item_count > delete_count {
            Self::move_range(a, o, start + delete_count, start + item_count, len - start - delete_count)?;
        }
        Ok(())
    }

    /// IsConcatSpreadable。
    fn spreadable<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<bool, Error> {
        if !matches!(v, Value::Object(_)) {
            return Ok(false);
        }
        let s = a.get_symbol(v, a.realm.symbols.is_concat_spreadable, v)?;
        if s != Value::Undefined {
            return Ok(a.to_boolean(s));
        }
        a.is_array(v)
    }

    fn concat<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = a.to_object(this)?;
        let arr = Self::species_create(a, o, 0.0)?;
        let mut n = 0.0;
        for e in core::iter::once(Value::Object(o)).chain(args.iter().copied()) {
            if Self::spreadable(a, e)? {
                let Value::Object(eo) = e else { unreachable!("spreadable values are objects") };
                let len = a.length_of_array_like(e)?;
                if n + len > MAX_LENGTH {
                    return Err(a.throw(ErrorKind::Type, "Concatenation exceeds 2^53 - 1 length"));
                }
                let mut k = 0.0;
                while k < len {
                    if a.has_index(eo, k)? {
                        let v = a.get_index(e, k)?;
                        a.create_index(arr, n, v)?;
                    }
                    n += 1.0;
                    k += 1.0;
                }
            } else {
                if n >= MAX_LENGTH {
                    return Err(a.throw(ErrorKind::Type, "Concatenation exceeds 2^53 - 1 length"));
                }
                a.create_index(arr, n, e)?;
                n += 1.0;
            }
        }
        Self::set_length(a, arr, n)?;
        Ok(arr.into())
    }

    fn join<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        let sep = match args.at(0) {
            Value::Undefined => a.string(","),
            v => a.to_string(v)?,
        };
        let mut out = String::new();
        let mut k = 0.0;
        while k < len {
            if k > 0.0 {
                out.push_str(&sep);
            }
            let v = a.get_index(o.into(), k)?;
            if !v.is_nullish() {
                out.push_str(&a.to_string(v)?);
            }
            k += 1.0;
        }
        Ok(a.string(&out).into())
    }

    fn to_string<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = a.to_object(this)?;
        let join = a.get_named(o.into(), "join")?;
        if a.is_callable(join) {
            return a.call(join, o.into(), &[]);
        }
        let f = a.get_named(a.realm.object_proto.into(), "toString")?;
        a.call(f, o.into(), &[])
    }

    fn to_locale_string<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        let mut out = String::new();
        let mut k = 0.0;
        while k < len {
            if k > 0.0 {
                out.push(',');
            }
            let v = a.get_index(o.into(), k)?;
            if !v.is_nullish() {
                let f = a.get_named(v, "toLocaleString")?;
                let s = a.call(f, v, &[])?;
                out.push_str(&a.to_string(s)?);
            }
            k += 1.0;
        }
        Ok(a.string(&out).into())
    }

    fn reverse<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        let middle = libm::floor(len / 2.0);
        let mut lower = 0.0;
        while lower != middle {
            let upper = len - lower - 1.0;
            let lower_exists = a.has_index(o, lower)?;
            let lower_value = if lower_exists { a.get_index(o.into(), lower)? } else { Value::Undefined };
            let upper_exists = a.has_index(o, upper)?;
            let upper_value = if upper_exists { a.get_index(o.into(), upper)? } else { Value::Undefined };
            match (lower_exists, upper_exists) {
                (true, true) => {
                    a.set_index(o.into(), lower, upper_value)?;
                    a.set_index(o.into(), upper, lower_value)?;
                }
                (false, true) => {
                    a.set_index(o.into(), lower, upper_value)?;
                    a.delete_index(o, upper)?;
                }
                (true, false) => {
                    a.delete_index(o, lower)?;
                    a.set_index(o.into(), upper, lower_value)?;
                }
                (false, false) => {}
            }
            lower += 1.0;
        }
        Ok(o.into())
    }

    fn index_of<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        if len == 0.0 {
            return Ok((-1.0).into());
        }
        let n = a.to_integer_or_infinity(args.at(1))?;
        if n == f64::INFINITY {
            return Ok((-1.0).into());
        }
        let mut k = if n >= 0.0 { n } else { (len + n).max(0.0) };
        let want = args.at(0);
        while k < len {
            if a.has_index(o, k)? && a.get_index(o.into(), k)? == want {
                return Ok(k.into());
            }
            k += 1.0;
        }
        Ok((-1.0).into())
    }

    fn last_index_of<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        if len == 0.0 {
            return Ok((-1.0).into());
        }
        let n = if args.len() > 1 { a.to_integer_or_infinity(args[1])? } else { len - 1.0 };
        if n == f64::NEG_INFINITY {
            return Ok((-1.0).into());
        }
        let mut k = if n >= 0.0 { n.min(len - 1.0) } else { len + n };
        let want = args.at(0);
        while k >= 0.0 {
            if a.has_index(o, k)? && a.get_index(o.into(), k)? == want {
                return Ok(k.into());
            }
            k -= 1.0;
        }
        Ok((-1.0).into())
    }

    /// SameValueZero：`NaN` 等于自己。
    fn same_value_zero<'gc>(x: Value<'gc>, y: Value<'gc>) -> bool {
        x == y || matches!((x, y), (Value::Number(p), Value::Number(q)) if p.is_nan() && q.is_nan())
    }

    fn includes<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        if len == 0.0 {
            return Ok(false.into());
        }
        let n = a.to_integer_or_infinity(args.at(1))?;
        if n == f64::INFINITY {
            return Ok(false.into());
        }
        let mut k = if n >= 0.0 { n } else { (len + n).max(0.0) };
        let want = args.at(0);
        while k < len {
            if Self::same_value_zero(a.get_index(o.into(), k)?, want) {
                return Ok(true.into());
            }
            k += 1.0;
        }
        Ok(false.into())
    }

    /// forEach / map / filter / some / every：跳过空洞，逐个调 `f(item, index, O)`。
    fn visit<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>], how: Visit) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        let (f, this_arg) = (args.at(0), args.at(1));
        Self::callback(a, f)?;
        let out = match how {
            Visit::Map => Some(Self::species_create(a, o, len)?),
            Visit::Filter => Some(Self::species_create(a, o, 0.0)?),
            _ => None,
        };
        let (mut k, mut to) = (0.0, 0.0);
        while k < len {
            if a.has_index(o, k)? {
                let v = a.get_index(o.into(), k)?;
                let r = a.call(f, this_arg, &[v, k.into(), o.into()])?;
                match (how, out) {
                    (Visit::Map, Some(arr)) => a.create_index(arr, k, r)?,
                    (Visit::Filter, Some(arr)) if a.to_boolean(r) => {
                        a.create_index(arr, to, v)?;
                        to += 1.0;
                    }
                    (Visit::Some, _) if a.to_boolean(r) => return Ok(true.into()),
                    (Visit::Every, _) if !a.to_boolean(r) => return Ok(false.into()),
                    _ => {}
                }
            }
            k += 1.0;
        }
        Ok(match (how, out) {
            (_, Some(arr)) => arr.into(),
            (Visit::Some, _) => false.into(),
            (Visit::Every, _) => true.into(),
            _ => Value::Undefined,
        })
    }

    fn for_each<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::visit(a, this, args, Visit::ForEach)
    }

    fn map<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::visit(a, this, args, Visit::Map)
    }

    fn filter<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::visit(a, this, args, Visit::Filter)
    }

    fn some<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::visit(a, this, args, Visit::Some)
    }

    fn every<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::visit(a, this, args, Visit::Every)
    }

    /// find / findIndex / findLast / findLastIndex：不跳空洞。
    fn find_by<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>], how: Find) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        let (f, this_arg) = (args.at(0), args.at(1));
        Self::callback(a, f)?;
        let backward = matches!(how, Find::LastValue | Find::LastIndex);
        let mut i = 0.0;
        while i < len {
            let k = if backward { len - 1.0 - i } else { i };
            let v = a.get_index(o.into(), k)?;
            let r = a.call(f, this_arg, &[v, k.into(), o.into()])?;
            if a.to_boolean(r) {
                return Ok(if matches!(how, Find::Value | Find::LastValue) { v } else { k.into() });
            }
            i += 1.0;
        }
        Ok(if matches!(how, Find::Value | Find::LastValue) { Value::Undefined } else { (-1.0).into() })
    }

    fn find<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::find_by(a, this, args, Find::Value)
    }

    fn find_index<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::find_by(a, this, args, Find::Index)
    }

    fn find_last<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::find_by(a, this, args, Find::LastValue)
    }

    fn find_last_index<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::find_by(a, this, args, Find::LastIndex)
    }

    /// reduce / reduceRight。
    fn fold<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>], right: bool) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        let f = args.at(0);
        Self::callback(a, f)?;
        let index = |i: f64| if right { len - 1.0 - i } else { i };
        let mut i = 0.0;
        let mut acc = if args.len() >= 2 {
            args[1]
        } else {
            loop {
                if i >= len {
                    return Err(a.throw(ErrorKind::Type, "Reduce of empty array with no initial value"));
                }
                let k = index(i);
                i += 1.0;
                if a.has_index(o, k)? {
                    break a.get_index(o.into(), k)?;
                }
            }
        };
        while i < len {
            let k = index(i);
            if a.has_index(o, k)? {
                let v = a.get_index(o.into(), k)?;
                acc = a.call(f, Value::Undefined, &[acc, v, k.into(), o.into()])?;
            }
            i += 1.0;
        }
        Ok(acc)
    }

    fn reduce<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::fold(a, this, args, false)
    }

    fn reduce_right<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::fold(a, this, args, true)
    }

    fn fill<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        let mut k = Self::relative(a, args.at(1), len, 0.0)?;
        let end = Self::relative(a, args.at(2), len, len)?;
        while k < end {
            a.set_index(o.into(), k, args.at(0))?;
            k += 1.0;
        }
        Ok(o.into())
    }

    fn copy_within<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        let to = Self::relative(a, args.at(0), len, 0.0)?;
        let from = Self::relative(a, args.at(1), len, 0.0)?;
        let end = Self::relative(a, args.at(2), len, len)?;
        let count = (end - from).min(len - to);
        if count > 0.0 {
            Self::move_range(a, o, from, to, count)?;
        }
        Ok(o.into())
    }

    /// FlattenIntoArray：给出下一个目标下标。
    #[expect(clippy::too_many_arguments, reason = "规范的抽象操作就是这些参数")]
    fn flatten<'gc>(a: &mut Agent<'gc, '_>, target: JsObject<'gc>, source: JsObject<'gc>, len: f64, start: f64, depth: f64, mapper: Value<'gc>, this_arg: Value<'gc>) -> Result<f64, Error> {
        let mut to = start;
        let mut k = 0.0;
        while k < len {
            if a.has_index(source, k)? {
                let mut v = a.get_index(source.into(), k)?;
                if mapper != Value::Undefined {
                    v = a.call(mapper, this_arg, &[v, k.into(), source.into()])?;
                }
                if depth > 0.0 && a.is_array(v)? {
                    let Value::Object(inner) = v else { unreachable!("arrays are objects") };
                    let inner_len = a.length_of_array_like(v)?;
                    to = Self::flatten(a, target, inner, inner_len, to, depth - 1.0, Value::Undefined, Value::Undefined)?;
                } else {
                    if to >= MAX_LENGTH {
                        return Err(a.throw(ErrorKind::Type, "Flattened array exceeds 2^53 - 1 length"));
                    }
                    a.create_index(target, to, v)?;
                    to += 1.0;
                }
            }
            k += 1.0;
        }
        Ok(to)
    }

    fn flat<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        let depth = match args.at(0) {
            Value::Undefined => 1.0,
            d => a.to_integer_or_infinity(d)?.max(0.0),
        };
        let arr = Self::species_create(a, o, 0.0)?;
        Self::flatten(a, arr, o, len, 0.0, depth, Value::Undefined, Value::Undefined)?;
        Ok(arr.into())
    }

    fn flat_map<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        Self::callback(a, args.at(0))?;
        let arr = Self::species_create(a, o, 0.0)?;
        Self::flatten(a, arr, o, len, 0.0, 1.0, args.at(0), args.at(1))?;
        Ok(arr.into())
    }

    /// SortCompare：`undefined` 排最后；有比较函数按它的符号（NaN 当 0），否则按 ToString 的 UTF-16 序。
    fn order<'gc>(a: &mut Agent<'gc, '_>, f: Value<'gc>, x: Value<'gc>, y: Value<'gc>) -> Result<Ordering, Error> {
        Ok(match (x, y) {
            (Value::Undefined, Value::Undefined) => Ordering::Equal,
            (Value::Undefined, _) => Ordering::Greater,
            (_, Value::Undefined) => Ordering::Less,
            _ if f != Value::Undefined => {
                let r = a.call(f, Value::Undefined, &[x, y])?;
                a.to_number(r)?.partial_cmp(&0.0).unwrap_or(Ordering::Equal)
            }
            _ => {
                let (s, t) = (a.to_string(x)?, a.to_string(y)?);
                s.encode_utf16().cmp(t.encode_utf16())
            }
        })
    }

    /// 稳定的归并排序（比较函数可能抛错，不能用 `sort_by`）。
    fn merge_sort<'gc>(a: &mut Agent<'gc, '_>, f: Value<'gc>, v: Vec<Value<'gc>>) -> Result<Vec<Value<'gc>>, Error> {
        if v.len() <= 1 {
            return Ok(v);
        }
        let mut right = v;
        let left = right.drain(..right.len() / 2).collect();
        let (left, right) = (Self::merge_sort(a, f, left)?, Self::merge_sort(a, f, right)?);
        let mut out = Vec::with_capacity(left.len() + right.len());
        let (mut l, mut r) = (left.into_iter().peekable(), right.into_iter().peekable());
        while let (Some(&x), Some(&y)) = (l.peek(), r.peek()) {
            if Self::order(a, f, x, y)? == Ordering::Greater {
                out.push(y);
                r.next();
            } else {
                out.push(x);
                l.next();
            }
        }
        out.extend(l);
        out.extend(r);
        Ok(out)
    }

    fn comparator<'gc>(a: &mut Agent<'gc, '_>, f: Value<'gc>) -> Result<(), Error> {
        if f != Value::Undefined && !a.is_callable(f) {
            return Err(a.throw(ErrorKind::Type, "The comparison function must be either a function or undefined"));
        }
        Ok(())
    }

    fn sort<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let f = args.at(0);
        Self::comparator(a, f)?;
        let (o, len) = Self::target(a, this)?;
        let mut items = Vec::new();
        let mut k = 0.0;
        while k < len {
            if a.has_index(o, k)? {
                items.push(a.get_index(o.into(), k)?);
            }
            k += 1.0;
        }
        let sorted = Self::merge_sort(a, f, items)?;
        #[expect(clippy::cast_precision_loss, reason = "元素个数远小于 2^53")]
        let count = sorted.len() as f64;
        for (j, v) in (0..).zip(sorted) {
            a.set_index(o.into(), f64::from(j), v)?;
        }
        let mut j = count;
        while j < len {
            a.delete_index(o, j)?;
            j += 1.0;
        }
        Ok(o.into())
    }

    fn to_sorted<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let f = args.at(0);
        Self::comparator(a, f)?;
        let (o, len) = Self::target(a, this)?;
        let arr = Self::create(a, len, None)?;
        let mut items = Vec::new();
        let mut k = 0.0;
        while k < len {
            items.push(a.get_index(o.into(), k)?);
            k += 1.0;
        }
        let sorted = Self::merge_sort(a, f, items)?;
        for (j, v) in (0..).zip(sorted) {
            a.create_index(arr, f64::from(j), v)?;
        }
        Ok(arr.into())
    }

    fn to_reversed<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        let arr = Self::create(a, len, None)?;
        let mut k = 0.0;
        while k < len {
            let v = a.get_index(o.into(), len - k - 1.0)?;
            a.create_index(arr, k, v)?;
            k += 1.0;
        }
        Ok(arr.into())
    }

    fn to_spliced<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        let start = Self::relative(a, args.at(0), len, 0.0)?;
        let items = args.get(2..).unwrap_or(&[]);
        #[expect(clippy::cast_precision_loss, reason = "实参个数远小于 2^53")]
        let insert = items.len() as f64;
        let skip = match args.len() {
            0 => 0.0,
            1 => len - start,
            _ => a.to_integer_or_infinity(args[1])?.clamp(0.0, len - start),
        };
        let new_len = len + insert - skip;
        if new_len > MAX_LENGTH {
            return Err(a.throw(ErrorKind::Type, "toSpliced result exceeds 2^53 - 1 length"));
        }
        let arr = Self::create(a, new_len, None)?;
        let mut i = 0.0;
        while i < start {
            let v = a.get_index(o.into(), i)?;
            a.create_index(arr, i, v)?;
            i += 1.0;
        }
        for &v in items {
            a.create_index(arr, i, v)?;
            i += 1.0;
        }
        let mut r = start + skip;
        while i < new_len {
            let v = a.get_index(o.into(), r)?;
            a.create_index(arr, i, v)?;
            i += 1.0;
            r += 1.0;
        }
        Ok(arr.into())
    }

    fn with<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (o, len) = Self::target(a, this)?;
        let rel = a.to_integer_or_infinity(args.at(0))?;
        let at = if rel >= 0.0 { rel } else { len + rel };
        if at >= len || at < 0.0 {
            return Err(a.throw(ErrorKind::Range, "Invalid index"));
        }
        let arr = Self::create(a, len, None)?;
        let mut k = 0.0;
        while k < len {
            let v = if k == at { args.at(1) } else { a.get_index(o.into(), k)? };
            a.create_index(arr, k, v)?;
            k += 1.0;
        }
        Ok(arr.into())
    }
}

impl Installer<'_, '_> {
    pub(crate) fn array(&mut self) {
        let proto = self.realm.array_proto;
        // Array.prototype 本身是个空数组
        proto.borrow_mut(self.mc).kind = ObjectKind::Array(Vec::new());
        let ctor = self.constructor("Array", Arrays::array, proto, 1);
        self.method(ctor, "isArray", Arrays::is_array, 1);
        self.method(ctor, "of", Arrays::of, 0);
        self.method(ctor, "from", Arrays::from, 1);
        let species = self.realm.named_native(self.mc, Arrays::species, "get [Symbol.species]", 0);
        ctor.borrow_mut(self.mc).define_symbol(self.realm.symbols.species, Property { slot: Slot::Accessor { get: Some(species), set: None }, enumerable: false, writable: false, configurable: true });
        let methods: [(&str, r8frame::NativeFn, u32); 34] = [
            ("at", Arrays::at, 1),
            ("concat", Arrays::concat, 1),
            ("copyWithin", Arrays::copy_within, 2),
            ("every", Arrays::every, 1),
            ("fill", Arrays::fill, 1),
            ("filter", Arrays::filter, 1),
            ("find", Arrays::find, 1),
            ("findIndex", Arrays::find_index, 1),
            ("findLast", Arrays::find_last, 1),
            ("findLastIndex", Arrays::find_last_index, 1),
            ("flat", Arrays::flat, 0),
            ("flatMap", Arrays::flat_map, 1),
            ("forEach", Arrays::for_each, 1),
            ("includes", Arrays::includes, 1),
            ("indexOf", Arrays::index_of, 1),
            ("join", Arrays::join, 1),
            ("lastIndexOf", Arrays::last_index_of, 1),
            ("map", Arrays::map, 1),
            ("pop", Arrays::pop, 0),
            ("push", Arrays::push, 1),
            ("reduce", Arrays::reduce, 1),
            ("reduceRight", Arrays::reduce_right, 1),
            ("reverse", Arrays::reverse, 0),
            ("shift", Arrays::shift, 0),
            ("slice", Arrays::slice, 2),
            ("some", Arrays::some, 1),
            ("sort", Arrays::sort, 1),
            ("splice", Arrays::splice, 2),
            ("toLocaleString", Arrays::to_locale_string, 0),
            ("toReversed", Arrays::to_reversed, 0),
            ("toSorted", Arrays::to_sorted, 1),
            ("toSpliced", Arrays::to_spliced, 2),
            ("toString", Arrays::to_string, 0),
            ("unshift", Arrays::unshift, 1),
        ];
        for (name, f, n) in methods {
            self.method(proto, name, f, n);
        }
        self.method(proto, "with", Arrays::with, 2);
        // `[Symbol.unscopables]`：`with` 语句里不暴露的方法名
        let unscopables = ObjectData::default().alloc(self.mc);
        for name in ["at", "copyWithin", "entries", "fill", "find", "findIndex", "findLast", "findLastIndex", "flat", "flatMap", "includes", "keys", "toReversed", "toSorted", "toSpliced", "values"] {
            unscopables.borrow_mut(self.mc).define(self.mc, name, Property::data(true.into()));
        }
        proto.borrow_mut(self.mc).define_symbol(self.realm.symbols.unscopables, Property { slot: Slot::Data(unscopables.into()), enumerable: false, writable: false, configurable: true });
    }
}
