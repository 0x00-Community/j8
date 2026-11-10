use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use gc_arena::{Gc, Static};
use r8frame::{Agent, Error, ErrorKind, JsObject, JsString, ObjectData, ObjectKind, Property, RegExpData, Slot, Value};

use crate::string::Strings;
use crate::{Args, Installer};

/// 合法的正则 flags
const FLAGS: &str = "dgimsuvy";

/// 交给正则引擎的 flags（其余是 JS 层的：`g` / `y` / `d`）
const ENGINE_FLAGS: &str = "imsuv";

/// 一次匹配：整体与各分组的 UTF-16 下标范围、具名分组。
struct Found {
    range: core::ops::Range<usize>,
    groups: Vec<Option<core::ops::Range<usize>>>,
    named: Vec<(String, Option<core::ops::Range<usize>>)>,
}

/// `RegExp` 与正则相关的字符串方法。下标都按 UTF-16 码元算。
struct Regexps;

impl Regexps {
    /// 编译：flags 先查合法再挑出引擎要的那几个。
    fn compile(a: &Agent<'_, '_>, source: &str, flags: &str) -> Result<regress::Regex, Error> {
        let bad = flags.chars().enumerate().any(|(i, c)| !FLAGS.contains(c) || flags[..i].contains(c));
        if bad {
            return Err(a.throw(ErrorKind::Syntax, &alloc::format!("Invalid flags supplied to RegExp constructor '{flags}'")));
        }
        let engine: String = flags.chars().filter(|&c| ENGINE_FLAGS.contains(c)).collect();
        regress::Regex::with_flags(source, engine.as_str()).map_err(|e| a.throw(ErrorKind::Syntax, &alloc::format!("Invalid regular expression: /{source}/{flags}: {e}")))
    }

    /// `new RegExp(pattern, flags)`：`pattern` 可以是正则（flags 缺省沿用它的）。
    fn construct<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (src, inherited) = match args.at(0) {
            Value::Object(o) => match &o.borrow().kind {
                ObjectKind::RegExp(d) => (Some(d.source), Some(d.flags)),
                _ => (None, None),
            },
            _ => (None, None),
        };
        let source = match (src, args.at(0)) {
            (Some(s), _) => s,
            (None, Value::Undefined) => a.string("(?:)"),
            (None, v) => a.to_string(v)?,
        };
        let flags = match (args.at(1), inherited) {
            (Value::Undefined, Some(f)) => f,
            (Value::Undefined, None) => a.string(""),
            (f, _) => a.to_string(f)?,
        };
        let regex = Self::compile(a, &source, &flags)?;
        let compiled = Gc::new(a.mc, Static(Box::new(regex) as Box<dyn core::any::Any>));
        let o = ObjectData { kind: ObjectKind::RegExp(RegExpData { source, flags, compiled }), proto: Some(a.realm.regexp_proto), ..ObjectData::default() }.alloc(a.mc);
        o.borrow_mut(a.mc).define(a.mc, "lastIndex", Property { slot: Slot::Data(0.0.into()), enumerable: false, writable: true, configurable: false });
        Ok(o.into())
    }

    /// 是正则对象就给出来。
    fn regexp(v: Value<'_>) -> Option<JsObject<'_>> {
        match v {
            Value::Object(o) if matches!(o.borrow().kind, ObjectKind::RegExp(_)) => Some(o),
            _ => None,
        }
    }

    fn this<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        Self::regexp(this).ok_or_else(|| a.throw(ErrorKind::Type, "RegExp.prototype method called on an incompatible receiver"))
    }

    fn data(o: JsObject<'_>) -> (JsString<'_>, JsString<'_>, Gc<'_, Static<Box<dyn core::any::Any>>>) {
        let ObjectKind::RegExp(d) = &o.borrow().kind else { unreachable!("checked to be a regexp") };
        (d.source, d.flags, d.compiled)
    }

    fn has_flag(o: JsObject<'_>, c: char) -> bool {
        Self::data(o).1.contains(c)
    }

    /// 从 `start` 起找第一个匹配（`u` / `v` 按码点，否则按码元）。
    fn find(o: JsObject<'_>, units: &[u16], start: usize) -> Option<Found> {
        let (_, flags, compiled) = Self::data(o);
        let re = compiled.0.downcast_ref::<regress::Regex>().expect("a regress regex");
        let m = if flags.contains('u') || flags.contains('v') { re.find_from_utf16(units, start).next() } else { re.find_from_ucs2(units, start).next() }?;
        let named = m.named_groups().map(|(n, r)| (String::from(n), r)).collect();
        Some(Found { range: m.range(), groups: m.captures, named })
    }

    fn text<'gc>(a: &Agent<'gc, '_>, units: &[u16], r: Option<core::ops::Range<usize>>) -> Value<'gc> {
        r.map_or(Value::Undefined, |r| a.string(&String::from_utf16_lossy(&units[r])).into())
    }

    fn index<'gc>(i: usize) -> Value<'gc> {
        Value::Number(f64::from(u32::try_from(i).unwrap_or(u32::MAX)))
    }

    /// RegExpBuiltinExec 的匹配部分：`g` / `y` 从 `lastIndex` 起并更新它，`y` 只认正好在那儿开始的。
    fn exec_raw<'gc>(a: &mut Agent<'gc, '_>, o: JsObject<'gc>, units: &[u16]) -> Result<Option<Found>, Error> {
        let (global, sticky) = (Self::has_flag(o, 'g'), Self::has_flag(o, 'y'));
        let last = if global || sticky {
            let li = a.get_named(o.into(), "lastIndex")?;
            let x = a.to_number(li)?;
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "夹到非负再取整")]
            let i = if x.is_nan() || x <= 0.0 { 0 } else { libm::trunc(x) as usize };
            i
        } else {
            0
        };
        let found = if last > units.len() { None } else { Self::find(o, units, last).filter(|m| !sticky || m.range.start == last) };
        if global || sticky {
            let next = found.as_ref().map_or(0, |m| m.range.end);
            a.set_named(o.into(), "lastIndex", Self::index(next))?;
        }
        Ok(found)
    }

    /// 匹配结果数组：整体与分组，带 `index` / `input` / `groups`。
    fn result<'gc>(a: &mut Agent<'gc, '_>, units: &[u16], input: JsString<'gc>, m: &Found) -> Result<Value<'gc>, Error> {
        let mut items = alloc::vec![Self::text(a, units, Some(m.range.clone()))];
        items.extend(m.groups.iter().map(|g| Self::text(a, units, g.clone())));
        let arr = a.array(items);
        a.set_named(arr.into(), "index", Self::index(m.range.start))?;
        a.set_named(arr.into(), "input", input.into())?;
        let groups = if m.named.is_empty() {
            Value::Undefined
        } else {
            let g = ObjectData::default().alloc(a.mc);
            for (n, r) in &m.named {
                let v = Self::text(a, units, r.clone());
                g.borrow_mut(a.mc).set(a.mc, n, v);
            }
            g.into()
        };
        a.set_named(arr.into(), "groups", groups)?;
        Ok(arr.into())
    }

    fn exec<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::this(a, this)?;
        let s = a.to_string(args.at(0))?;
        let units: Vec<u16> = s.encode_utf16().collect();
        match Self::exec_raw(a, o, &units)? {
            Some(m) => Self::result(a, &units, s, &m),
            None => Ok(Value::Null),
        }
    }

    fn test<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::this(a, this)?;
        let s = a.to_string(args.at(0))?;
        let units: Vec<u16> = s.encode_utf16().collect();
        Ok(Self::exec_raw(a, o, &units)?.is_some().into())
    }

    fn to_string<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (source, flags, _) = Self::data(Self::this(a, this)?);
        Ok(a.string(&alloc::format!("/{}/{}", &**source, &**flags)).into())
    }

    fn source<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(Self::data(Self::this(a, this)?).0.into())
    }

    fn flags<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        // 规范顺序 d g i m s u v y
        let f = Self::data(Self::this(a, this)?).1;
        let sorted: String = FLAGS.chars().filter(|&c| f.contains(c)).collect();
        Ok(a.string(&sorted).into())
    }

    fn flag<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>, c: char) -> Result<Value<'gc>, Error> {
        Ok(Self::has_flag(Self::this(a, this)?, c).into())
    }

    fn global<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::flag(a, this, 'g')
    }

    fn ignore_case<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::flag(a, this, 'i')
    }

    fn multiline<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::flag(a, this, 'm')
    }

    fn dot_all<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::flag(a, this, 's')
    }

    fn unicode<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::flag(a, this, 'u')
    }

    fn sticky<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::flag(a, this, 'y')
    }

    /// 字符串方法的 `this`。
    fn subject<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>) -> Result<JsString<'gc>, Error> {
        if this.is_nullish() {
            return Err(a.throw(ErrorKind::Type, "String.prototype method called on null or undefined"));
        }
        a.to_string(this)
    }

    /// 实参是正则就用它，否则按它的字符串新建一个（`flags` 给的 flags）。
    fn coerce<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>, flags: &str) -> Result<JsObject<'gc>, Error> {
        if let Some(o) = Self::regexp(v) {
            return Ok(o);
        }
        let f = a.string(flags);
        let made = Self::construct(a, Value::Undefined, &[v, f.into()])?;
        Ok(Self::regexp(made).expect("just constructed"))
    }

    /// 全局匹配：从 0 开始逐个找，空匹配往前挪一格。
    fn all_matches(o: JsObject<'_>, units: &[u16]) -> Vec<Found> {
        let mut out = Vec::new();
        let mut at = 0;
        while at <= units.len() {
            let Some(m) = Self::find(o, units, at) else { break };
            at = if m.range.is_empty() { m.range.end + 1 } else { m.range.end };
            out.push(m);
        }
        out
    }

    fn string_match<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let s = Self::subject(a, this)?;
        let o = Self::coerce(a, args.at(0), "")?;
        let units: Vec<u16> = s.encode_utf16().collect();
        if !Self::has_flag(o, 'g') {
            return Self::exec(a, o.into(), &[s.into()]);
        }
        a.set_named(o.into(), "lastIndex", 0.0.into())?;
        let found = Self::all_matches(o, &units);
        if found.is_empty() {
            return Ok(Value::Null);
        }
        let items = found.into_iter().map(|m| Self::text(a, &units, Some(m.range))).collect();
        Ok(a.array(items).into())
    }

    /// `matchAll`：正则必须带 `g`；结果先算好再给数组迭代器。
    fn match_all<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let s = Self::subject(a, this)?;
        if let Some(o) = Self::regexp(args.at(0))
            && !Self::has_flag(o, 'g')
        {
            return Err(a.throw(ErrorKind::Type, "String.prototype.matchAll called with a non-global RegExp argument"));
        }
        let o = Self::coerce(a, args.at(0), "g")?;
        let units: Vec<u16> = s.encode_utf16().collect();
        let found = Self::all_matches(o, &units);
        let mut items = Vec::with_capacity(found.len());
        for m in &found {
            items.push(Self::result(a, &units, s, m)?);
        }
        let arr = a.array(items);
        Ok(a.array_iterator(arr.into(), r8frame::IterKind::Values).into())
    }

    fn search<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let s = Self::subject(a, this)?;
        let o = Self::coerce(a, args.at(0), "")?;
        let units: Vec<u16> = s.encode_utf16().collect();
        Ok(Self::find(o, &units, 0).map_or(Value::Number(-1.0), |m| Self::index(m.range.start)))
    }

    /// 替换串里的 `$$` / `$&` / `` $` `` / `$'` / `$n` / `$nn` / `$<name>`。
    fn expand(units: &[u16], m: &Found, template: &[u16]) -> Vec<u16> {
        let dollar = u16::from(b'$');
        let digit = |c: u16| char::from_u32(u32::from(c)).and_then(|c| c.to_digit(10));
        let group = |n: usize| -> Option<&[u16]> { m.groups.get(n.checked_sub(1)?)?.as_ref().map(|r| &units[r.clone()]) };
        let mut out = Vec::new();
        let mut i = 0;
        while i < template.len() {
            let c = template[i];
            let next = template.get(i + 1).copied();
            if c != dollar || next.is_none() {
                out.push(c);
                i += 1;
                continue;
            }
            let n = next.expect("checked above");
            match char::from_u32(u32::from(n)) {
                Some('$') => {
                    out.push(dollar);
                    i += 2;
                }
                Some('&') => {
                    out.extend_from_slice(&units[m.range.clone()]);
                    i += 2;
                }
                Some('`') => {
                    out.extend_from_slice(&units[..m.range.start]);
                    i += 2;
                }
                Some('\'') => {
                    out.extend_from_slice(&units[m.range.end..]);
                    i += 2;
                }
                Some('<') if !m.named.is_empty() => {
                    let close = template[i + 2..].iter().position(|&c| c == u16::from(b'>'));
                    match close {
                        Some(len) => {
                            let name = String::from_utf16_lossy(&template[i + 2..i + 2 + len]);
                            if let Some((_, Some(r))) = m.named.iter().find(|(n, _)| *n == name) {
                                out.extend_from_slice(&units[r.clone()]);
                            }
                            i += len + 3;
                        }
                        None => {
                            out.push(c);
                            i += 1;
                        }
                    }
                }
                _ => match digit(n) {
                    Some(d1) => {
                        // 两位数分组存在就用两位
                        let two = template.get(i + 2).copied().and_then(digit).map(|d2| (d1 * 10 + d2) as usize).filter(|&k| k >= 1 && k <= m.groups.len());
                        match two {
                            Some(k) => {
                                out.extend_from_slice(group(k).unwrap_or(&[]));
                                i += 3;
                            }
                            None if (d1 as usize) >= 1 && (d1 as usize) <= m.groups.len() => {
                                out.extend_from_slice(group(d1 as usize).unwrap_or(&[]));
                                i += 2;
                            }
                            None => {
                                out.push(c);
                                i += 1;
                            }
                        }
                    }
                    None => {
                        out.push(c);
                        i += 1;
                    }
                },
            }
        }
        out
    }

    /// 一处匹配的替换文本：函数就调它，否则展开模板。
    fn replacement<'gc>(a: &mut Agent<'gc, '_>, units: &[u16], input: JsString<'gc>, m: &Found, with: Value<'gc>) -> Result<Vec<u16>, Error> {
        if a.is_callable(with) {
            let mut args = alloc::vec![Self::text(a, units, Some(m.range.clone()))];
            args.extend(m.groups.iter().map(|g| Self::text(a, units, g.clone())));
            args.push(Self::index(m.range.start));
            args.push(input.into());
            if !m.named.is_empty() {
                let g = ObjectData::default().alloc(a.mc);
                for (n, r) in &m.named {
                    let v = Self::text(a, units, r.clone());
                    g.borrow_mut(a.mc).set(a.mc, n, v);
                }
                args.push(g.into());
            }
            let r = a.call(with, Value::Undefined, &args)?;
            return Ok(a.to_string(r)?.encode_utf16().collect());
        }
        let t: Vec<u16> = a.to_string(with)?.encode_utf16().collect();
        Ok(Self::expand(units, m, &t))
    }

    /// 字符串模式的全部（或第一处）出现。
    fn literal_matches(units: &[u16], needle: &[u16], all: bool) -> Vec<Found> {
        let mut out = Vec::new();
        let mut at = 0;
        while at <= units.len() {
            let Some(i) = (at..=units.len().saturating_sub(needle.len())).find(|&i| units[i..].starts_with(needle)) else { break };
            out.push(Found { range: i..i + needle.len(), groups: Vec::new(), named: Vec::new() });
            if !all {
                break;
            }
            at = i + needle.len().max(1);
        }
        out
    }

    fn replace_impl<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>], all: bool) -> Result<Value<'gc>, Error> {
        let s = Self::subject(a, this)?;
        let units: Vec<u16> = s.encode_utf16().collect();
        let found = match Self::regexp(args.at(0)) {
            Some(o) => {
                let global = Self::has_flag(o, 'g');
                if all && !global {
                    return Err(a.throw(ErrorKind::Type, "replaceAll must be called with a global RegExp"));
                }
                if global {
                    a.set_named(o.into(), "lastIndex", 0.0.into())?;
                    Self::all_matches(o, &units)
                } else {
                    Self::exec_raw(a, o, &units)?.into_iter().collect()
                }
            }
            None => {
                let needle: Vec<u16> = a.to_string(args.at(0))?.encode_utf16().collect();
                Self::literal_matches(&units, &needle, all)
            }
        };
        let mut out = Vec::with_capacity(units.len());
        let mut last = 0;
        for m in &found {
            out.extend_from_slice(&units[last..m.range.start]);
            out.extend(Self::replacement(a, &units, s, m, args.at(1))?);
            last = m.range.end;
        }
        out.extend_from_slice(&units[last..]);
        Ok(a.string(&String::from_utf16_lossy(&out)).into())
    }

    fn replace<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::replace_impl(a, this, args, false)
    }

    fn replace_all<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::replace_impl(a, this, args, true)
    }

    /// `split`：分隔符是正则就按匹配拆（分组也放进结果），否则交给字符串版。
    fn split<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let Some(o) = Self::regexp(args.at(0)) else { return Strings::split(a, this, args) };
        let s = Self::subject(a, this)?;
        let units: Vec<u16> = s.encode_utf16().collect();
        let limit = match args.at(1) {
            Value::Undefined => usize::MAX,
            v => a.to_uint32(v)? as usize,
        };
        let mut out: Vec<Value<'gc>> = Vec::new();
        if limit == 0 {
            return Ok(a.array(out).into());
        }
        if units.is_empty() {
            if Self::find(o, &units, 0).is_none() {
                out.push(s.into());
            }
            return Ok(a.array(out).into());
        }
        let (mut p, mut q) = (0, 0);
        while q < units.len() {
            let Some(m) = Self::find(o, &units, q) else { break };
            let (start, end) = (m.range.start, m.range.end.min(units.len()));
            if start >= units.len() {
                break;
            }
            if end == p {
                q = start + 1;
                continue;
            }
            out.push(Self::text(a, &units, Some(p..start)));
            for g in &m.groups {
                out.push(Self::text(a, &units, g.clone()));
            }
            if out.len() >= limit {
                out.truncate(limit);
                return Ok(a.array(out).into());
            }
            p = end;
            q = if end == start { end + 1 } else { end };
        }
        out.push(Self::text(a, &units, Some(p..units.len())));
        out.truncate(limit);
        Ok(a.array(out).into())
    }
}

impl Installer<'_, '_> {
    pub(crate) fn regexp(&mut self) {
        let proto = self.realm.regexp_proto;
        self.constructor("RegExp", Regexps::construct, proto);
        // 正则字面量用的内部构造器
        self.realm.regexp_ctor.borrow_mut(self.mc).kind = ObjectKind::Native(r8frame::Native(Regexps::construct));
        self.method(proto, "exec", Regexps::exec);
        self.method(proto, "test", Regexps::test);
        self.method(proto, "toString", Regexps::to_string);
        let getters: [(&str, r8frame::NativeFn); 8] = [
            ("source", Regexps::source),
            ("flags", Regexps::flags),
            ("global", Regexps::global),
            ("ignoreCase", Regexps::ignore_case),
            ("multiline", Regexps::multiline),
            ("dotAll", Regexps::dot_all),
            ("unicode", Regexps::unicode),
            ("sticky", Regexps::sticky),
        ];
        for (name, f) in getters {
            self.getter(proto, name, f);
        }
        let s = self.realm.string_proto;
        self.method(s, "match", Regexps::string_match);
        self.method(s, "matchAll", Regexps::match_all);
        self.method(s, "search", Regexps::search);
        self.method(s, "replace", Regexps::replace);
        self.method(s, "replaceAll", Regexps::replace_all);
        self.method(s, "split", Regexps::split);
    }
}
