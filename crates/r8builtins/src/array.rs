use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;

use r8frame::{Agent, Error, ErrorKind, JsObject, ObjectKind, Value};

use crate::{Args, Installer};

/// `Array` 与 `Array.prototype`。
struct Arrays;

impl Arrays {
    /// `this` 必须是数组。
    fn this<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        match this {
            Value::Object(o) if matches!(o.borrow().kind, ObjectKind::Array(_)) => Ok(o),
            _ => Err(a.throw(ErrorKind::Type, "Array.prototype method called on a non-array")),
        }
    }

    /// 元素的快照（回调里改数组不影响这次遍历的长度）。
    fn items<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<Vec<Value<'gc>>, Error> {
        let o = Self::this(a, this)?;
        let ObjectKind::Array(items) = &o.borrow().kind else { unreachable!("checked to be an array") };
        Ok(items.clone())
    }

    fn with_items<'gc, R>(a: &Agent<'gc, '_>, this: Value<'gc>, f: impl FnOnce(&mut Vec<Value<'gc>>) -> R) -> Result<R, Error> {
        let o = Self::this(a, this)?;
        let ObjectKind::Array(items) = &mut o.borrow_mut(a.mc).kind else { unreachable!("checked to be an array") };
        Ok(f(items))
    }

    fn index<'gc>(i: usize) -> Value<'gc> {
        Value::Number(f64::from(u32::try_from(i).unwrap_or(u32::MAX)))
    }

    /// 相对下标（负数从末尾数），夹到 `[0, len]`。
    fn relative<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>, len: usize, default: usize) -> Result<usize, Error> {
        if v == Value::Undefined {
            return Ok(default);
        }
        let x = libm::trunc(a.to_number(v)?);
        let len_f = f64::from(u32::try_from(len).unwrap_or(u32::MAX));
        let r = if x < 0.0 { (len_f + x).max(0.0) } else { x.min(len_f) };
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "已夹到 [0, len]")]
        let r = if r.is_nan() { 0 } else { r as usize };
        Ok(r)
    }

    fn array<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(a.array(args.to_vec()).into())
    }

    fn is_array<'gc>(_: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(matches!(args.at(0), Value::Object(o) if matches!(o.borrow().kind, ObjectKind::Array(_))).into())
    }

    fn push<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let n = Self::with_items(a, this, |items| {
            items.extend_from_slice(args);
            items.len()
        })?;
        Ok(Self::index(n))
    }

    fn pop<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(Self::with_items(a, this, Vec::pop)?.unwrap_or_default())
    }

    fn shift<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::with_items(a, this, |items| if items.is_empty() { Value::Undefined } else { items.remove(0) })
    }

    fn unshift<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let n = Self::with_items(a, this, |items| {
            items.splice(0..0, args.iter().copied());
            items.len()
        })?;
        Ok(Self::index(n))
    }

    fn slice<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let items = Self::items(a, this)?;
        let start = Self::relative(a, args.at(0), items.len(), 0)?;
        let end = Self::relative(a, args.at(1), items.len(), items.len())?;
        Ok(a.array(items.get(start..end.max(start)).unwrap_or(&[]).to_vec()).into())
    }

    fn concat<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let mut out = Self::items(a, this)?;
        for &v in args {
            match v {
                Value::Object(o) if matches!(o.borrow().kind, ObjectKind::Array(_)) => out.extend(Self::items(a, v)?),
                v => out.push(v),
            }
        }
        Ok(a.array(out).into())
    }

    fn join<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let sep = match args.at(0) {
            Value::Undefined => a.string(","),
            v => a.to_string(v)?,
        };
        let items = Self::items(a, this)?;
        let mut out = String::new();
        for (i, v) in items.into_iter().enumerate() {
            if i > 0 {
                out.push_str(&sep);
            }
            if !v.is_nullish() {
                out.push_str(&a.to_string(v)?);
            }
        }
        Ok(a.string(&out).into())
    }

    fn to_string<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::join(a, this, &[])
    }

    fn reverse<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::with_items(a, this, |items| items.reverse())?;
        Ok(this)
    }

    fn index_of<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let items = Self::items(a, this)?;
        Ok(items.iter().position(|&v| v == args.at(0)).map_or(Value::Number(-1.0), Self::index))
    }

    /// `includes` 用 SameValueZero：`NaN` 能找到自己。
    fn includes<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let want = args.at(0);
        let items = Self::items(a, this)?;
        Ok(items.iter().any(|&v| v == want || matches!((v, want), (Value::Number(x), Value::Number(y)) if x.is_nan() && y.is_nan())).into())
    }

    /// 逐个调回调 `f(item, index, array)`。
    fn each<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, f: Value<'gc>, mut visit: impl FnMut(&mut Agent<'gc, '_>, usize, Value<'gc>, Value<'gc>) -> Result<bool, Error>) -> Result<(), Error> {
        if !a.is_callable(f) {
            return Err(a.throw(ErrorKind::Type, "callback is not a function"));
        }
        for (i, v) in Self::items(a, this)?.into_iter().enumerate() {
            let r = a.call(f, Value::Undefined, &[v, Self::index(i), this])?;
            if !visit(a, i, v, r)? {
                break;
            }
        }
        Ok(())
    }

    fn for_each<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::each(a, this, args.at(0), |_, _, _, _| Ok(true))?;
        Ok(Value::Undefined)
    }

    fn map<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let mut out = Vec::new();
        Self::each(a, this, args.at(0), |_, _, _, r| {
            out.push(r);
            Ok(true)
        })?;
        Ok(a.array(out).into())
    }

    fn filter<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let mut out = Vec::new();
        Self::each(a, this, args.at(0), |a, _, v, r| {
            if a.to_boolean(r) {
                out.push(v);
            }
            Ok(true)
        })?;
        Ok(a.array(out).into())
    }

    fn some<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let mut hit = false;
        Self::each(a, this, args.at(0), |a, _, _, r| {
            hit = a.to_boolean(r);
            Ok(!hit)
        })?;
        Ok(hit.into())
    }

    fn every<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let mut all = true;
        Self::each(a, this, args.at(0), |a, _, _, r| {
            all = a.to_boolean(r);
            Ok(all)
        })?;
        Ok(all.into())
    }

    fn find<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let mut found = Value::Undefined;
        Self::each(a, this, args.at(0), |a, _, v, r| {
            let hit = a.to_boolean(r);
            if hit {
                found = v;
            }
            Ok(!hit)
        })?;
        Ok(found)
    }

    fn find_index<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let mut found = Value::Number(-1.0);
        Self::each(a, this, args.at(0), |a, i, _, r| {
            let hit = a.to_boolean(r);
            if hit {
                found = Self::index(i);
            }
            Ok(!hit)
        })?;
        Ok(found)
    }

    /// `reduce(f, init?)`。
    fn reduce<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let f = args.at(0);
        if !a.is_callable(f) {
            return Err(a.throw(ErrorKind::Type, "callback is not a function"));
        }
        let items = Self::items(a, this)?;
        let mut it = items.into_iter().enumerate();
        let mut acc = if args.len() >= 2 {
            args[1]
        } else {
            it.next().map(|(_, v)| v).ok_or_else(|| a.throw(ErrorKind::Type, "Reduce of empty array with no initial value"))?
        };
        for (i, v) in it {
            acc = a.call(f, Value::Undefined, &[acc, v, Self::index(i), this])?;
        }
        Ok(acc)
    }

    /// 两个元素的先后：有比较函数按它的符号，否则按 ToString 的 UTF-16 序；`undefined` 排最后。
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

    fn sort<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let f = args.at(0);
        if f != Value::Undefined && !a.is_callable(f) {
            return Err(a.throw(ErrorKind::Type, "The comparison function must be either a function or undefined"));
        }
        let items = Self::items(a, this)?;
        let sorted = Self::merge_sort(a, f, items)?;
        Self::with_items(a, this, |items| *items = sorted)?;
        Ok(this)
    }
}

impl Installer<'_, '_> {
    pub(crate) fn array(&mut self) {
        let proto = self.realm.array_proto;
        let ctor = self.constructor("Array", Arrays::array, proto);
        self.method(ctor, "isArray", Arrays::is_array);
        self.method(ctor, "of", Arrays::array);
        let methods: [(&str, r8frame::NativeFn); 23] = [
            ("push", Arrays::push),
            ("pop", Arrays::pop),
            ("shift", Arrays::shift),
            ("unshift", Arrays::unshift),
            ("slice", Arrays::slice),
            ("concat", Arrays::concat),
            ("join", Arrays::join),
            ("toString", Arrays::to_string),
            ("reverse", Arrays::reverse),
            ("indexOf", Arrays::index_of),
            ("includes", Arrays::includes),
            ("forEach", Arrays::for_each),
            ("map", Arrays::map),
            ("filter", Arrays::filter),
            ("some", Arrays::some),
            ("every", Arrays::every),
            ("find", Arrays::find),
            ("findIndex", Arrays::find_index),
            ("reduce", Arrays::reduce),
            ("sort", Arrays::sort),
            ("at", Arrays::at),
            ("fill", Arrays::fill),
            ("lastIndexOf", Arrays::last_index_of),
        ];
        for (name, f) in methods {
            self.method(proto, name, f);
        }
    }
}

impl Arrays {
    fn at<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let items = Self::items(a, this)?;
        let x = libm::trunc(a.to_number(args.at(0))?);
        let len = f64::from(u32::try_from(items.len()).unwrap_or(u32::MAX));
        let i = if x < 0.0 { len + x } else { x };
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "已检查在 [0, len) 里")]
        let v = if (0.0..len).contains(&i) { items[i as usize] } else { Value::Undefined };
        Ok(v)
    }

    fn fill<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let len = Self::items(a, this)?.len();
        let start = Self::relative(a, args.at(1), len, 0)?;
        let end = Self::relative(a, args.at(2), len, len)?;
        let v = args.at(0);
        Self::with_items(a, this, |items| items.get_mut(start..end.max(start)).into_iter().flatten().for_each(|x| *x = v))?;
        Ok(this)
    }

    fn last_index_of<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let items = Self::items(a, this)?;
        Ok(items.iter().rposition(|&v| v == args.at(0)).map_or(Value::Number(-1.0), Self::index))
    }
}
