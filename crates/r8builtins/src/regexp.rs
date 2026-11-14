use alloc::boxed::Box;
use core::fmt::Write;
use alloc::string::String;
use alloc::vec::Vec;

use r8gc::{Gc, Static};
use r8framework::{Agent, Error, ErrorKind, JsObject, JsString, Key, ObjectData, ObjectKind, Property, RegExpData, Slot, Units, Value};

use crate::buffer::Buffers;
use crate::string::Strings;
use crate::{Args, Installer};

/// Valid regex flags
const FLAGS: &str = "dgimsuvy";

/// Flags passed to the regex engine (the rest are handled at the JS level: `g` / `y` / `d`)
const ENGINE_FLAGS: &str = "imsuv";

/// Properties read by the `flags` getter, in spec order.
const FLAG_PROPERTIES: [(char, &str); 8] = [('d', "hasIndices"), ('g', "global"), ('i', "ignoreCase"), ('m', "multiline"), ('s', "dotAll"), ('u', "unicode"), ('v', "unicodeSets"), ('y', "sticky")];

/// 2^32 − 1: default limit of `split`
const MAX_SPLIT: f64 = 4_294_967_295.0;

/// One match: UTF-16 index ranges of the whole match and of each group, plus named groups.
struct Found {
    range: core::ops::Range<usize>,
    groups: Vec<Option<core::ops::Range<usize>>>,
    named: Vec<(String, Option<core::ops::Range<usize>>)>,
}

/// Compiled regex: the engine's regex, plus the literal text when the whole pattern is a single literal (searched as a
/// plain substring, bypassing the backtracking engine).
struct Compiled {
    regex: regress::Regex,
    atom: Option<Box<str>>,
}

impl Compiled {
    /// Syntax characters in a pattern: if there are none (and case is not ignored), the pattern is a literal
    const SYNTAX: &'static str = "\\^$.|?*+()[]{}/";

    fn new(regex: regress::Regex, source: &str, flags: &str) -> Self {
        let atom = (!source.is_empty() && !flags.contains('i') && !source.contains(|c| Self::SYNTAX.contains(c))).then(|| source.into());
        Self { regex, atom }
    }
}

/// `RegExp` and the regex-related string methods. All indices are in UTF-16 code units.
pub(crate) struct Regexps;

impl Regexps {
    /// Compile: validate the flags first, then pick out the ones the engine needs.
    fn compile(a: &Agent<'_, '_>, source: &str, flags: &str) -> Result<regress::Regex, Error> {
        let bad = flags.chars().enumerate().any(|(i, c)| !FLAGS.contains(c) || flags[..i].contains(c)) || (flags.contains('u') && flags.contains('v'));
        if bad {
            return Err(a.throw(ErrorKind::Syntax, &alloc::format!("Invalid flags supplied to RegExp constructor '{flags}'")));
        }
        let engine: String = flags.chars().filter(|&c| ENGINE_FLAGS.contains(c)).collect();
        regress::Regex::with_flags(source, engine.as_str()).map_err(|e| a.throw(ErrorKind::Syntax, &alloc::format!("Invalid regular expression: /{source}/{flags}: {e}")))
    }

    /// Compiled regex: look up the realm's cache first (identical source and flags share one copy); otherwise compile
    /// it and record it.
    fn compiled<'gc>(a: &Agent<'gc, '_>, source: &str, flags: &str) -> Result<Gc<'gc, Static<Box<dyn core::any::Any>>>, Error> {
        /// Maximum number of entries in the cache
        const CACHE_SIZE: usize = 64;
        let cache = a.realm.regexp_cache;
        let hit = cache.borrow().iter().position(|c| *c.source == *source && *c.flags == *flags);
        if let Some(i) = hit {
            let mut c = cache.borrow_mut(a.mc);
            let entry = c.remove(i).expect("just found");
            let compiled = entry.compiled;
            c.push_back(entry);
            return Ok(compiled);
        }
        let regex = Compiled::new(Self::compile(a, source, flags)?, source, flags);
        let compiled = Gc::new(a.mc, Static(Box::new(regex) as Box<dyn core::any::Any>));
        let mut c = cache.borrow_mut(a.mc);
        if c.len() >= CACHE_SIZE {
            c.pop_front();
        }
        c.push_back(r8framework::RegExpCompiled { source: source.into(), flags: flags.into(), compiled });
        Ok(compiled)
    }

    /// IsRegExp: check `Symbol.match`; if absent, check whether it is a RegExp object.
    pub(crate) fn is_regexp<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<bool, Error> {
        if !matches!(v, Value::Object(_)) {
            return Ok(false);
        }
        let m = a.get_symbol(v, a.realm.symbols.match_, v)?;
        if m != Value::Undefined {
            return Ok(a.to_boolean(m));
        }
        Ok(Self::regexp(v).is_some())
    }

    fn is_regexp_ctor(v: Value<'_>) -> bool {
        matches!(v, Value::Object(f) if matches!(f.borrow().kind, ObjectKind::Native(n) if core::ptr::fn_addr_eq(n.f, Self::construct as r8framework::NativeFn)))
    }

    /// RegExpInitialize: compile and install into `o`, setting `lastIndex` to 0.
    fn initialize<'gc>(a: &mut Agent<'gc, '_>, o: JsObject<'gc>, pattern: Value<'gc>, flags: Value<'gc>) -> Result<(), Error> {
        let source = if pattern == Value::Undefined { a.string("") } else { a.to_string(pattern)? };
        let flags = if flags == Value::Undefined { a.string("") } else { a.to_string(flags)? };
        let compiled = Self::compiled(a, &source, &flags)?;
        let legacy = matches!(&o.borrow().kind, ObjectKind::RegExp(d) if d.legacy);
        o.borrow_mut(a.mc).kind = ObjectKind::RegExp(RegExpData { source, flags, compiled, legacy });
        a.set_named(o.into(), "lastIndex", 0.0.into())
    }

    /// RegExpAlloc: prototype taken from new.target; `lastIndex` is writable, non-enumerable, non-configurable.
    fn alloc<'gc>(a: &mut Agent<'gc, '_>, new_target: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        let proto = a.proto_from_ctor(new_target, a.realm.regexp_proto)?;
        // Start with an empty regex as a placeholder, and record whether Annex B `compile` may be used (not on subclass
        // instances)
        let legacy = new_target == Value::Undefined || Self::is_regexp_ctor(new_target);
        let empty = a.string("");
        let compiled = Self::compiled(a, "", "")?;
        let kind = ObjectKind::RegExp(RegExpData { source: empty, flags: empty, compiled, legacy });
        let o = ObjectData { kind, proto: Some(proto), ..ObjectData::default() }.alloc(a.mc);
        o.borrow_mut(a.mc).define("lastIndex", Property { slot: Slot::Data(Value::Undefined), enumerable: false, writable: true, configurable: false });
        Ok(o)
    }

    /// RegExpCreate(P, F).
    fn create<'gc>(a: &mut Agent<'gc, '_>, pattern: Value<'gc>, flags: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        let o = Self::alloc(a, Value::Undefined)?;
        Self::initialize(a, o, pattern, flags)?;
        Ok(o)
    }

    /// `RegExp(pattern, flags)` / `new RegExp(…)`.
    fn construct<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (pattern, flags) = (args.at(0), args.at(1));
        // Fast path: plain call, source and flags both strings (this is how regex literals are created). RegExpAlloc +
        // RegExpInitialize merged into one step: the prototype is this realm's `RegExp.prototype`; compile first, then
        // create the object, and define `lastIndex` directly as 0 (a Set on a fresh object is indistinguishable)
        if a.new_target == Value::Undefined
            && let (Value::String(source), Value::String(flags)) = (pattern, flags)
        {
            let compiled = Self::compiled(a, &source, &flags)?;
            let kind = ObjectKind::RegExp(RegExpData { source, flags, compiled, legacy: true });
            let o = ObjectData { kind, proto: Some(a.realm.regexp_proto), ..ObjectData::default() }.alloc(a.mc);
            o.borrow_mut(a.mc).define("lastIndex", Property { slot: Slot::Data(0.0.into()), enumerable: false, writable: true, configurable: false });
            return Ok(o.into());
        }
        let pattern_is_regexp = Self::is_regexp(a, pattern)?;
        let mut new_target = a.new_target;
        if new_target == Value::Undefined {
            if pattern_is_regexp && flags == Value::Undefined {
                let ctor = a.get_named(pattern, "constructor")?;
                if Self::is_regexp_ctor(ctor) {
                    return Ok(pattern);
                }
            }
            new_target = Value::Undefined;
        }
        let (p, f) = if let Some(o) = Self::regexp(pattern) {
            let (source, original, _) = Self::data(o);
            (source.into(), if flags == Value::Undefined { original.into() } else { flags })
        } else if pattern_is_regexp {
            let source = a.get_named(pattern, "source")?;
            let f = if flags == Value::Undefined { a.get_named(pattern, "flags")? } else { flags };
            (source, f)
        } else {
            (pattern, flags)
        };
        let o = Self::alloc(a, new_target)?;
        Self::initialize(a, o, p, f)?;
        Ok(o.into())
    }

    /// `RegExp.prototype.compile(pattern, flags)` (annexB).
    fn compile_method<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::this(a, this)?;
        if !matches!(&o.borrow().kind, ObjectKind::RegExp(d) if d.legacy) {
            return Err(a.throw(ErrorKind::Type, "RegExp.prototype.compile cannot be used on RegExp subclass instances"));
        }
        let (p, f) = match Self::regexp(args.at(0)) {
            Some(src) => {
                if args.at(1) != Value::Undefined {
                    return Err(a.throw(ErrorKind::Type, "Cannot supply flags when constructing one RegExp from another"));
                }
                let (s, fl, _) = Self::data(src);
                (s.into(), fl.into())
            }
            None => (args.at(0), args.at(1)),
        };
        Self::initialize(a, o, p, f)?;
        Ok(o.into())
    }

    /// Returns it if it is a RegExp object.
    fn regexp(v: Value<'_>) -> Option<JsObject<'_>> {
        match v {
            Value::Object(o) if matches!(o.borrow().kind, ObjectKind::RegExp(_)) => Some(o),
            _ => None,
        }
    }

    fn this<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        Self::regexp(this).ok_or_else(|| a.throw(ErrorKind::Type, "RegExp.prototype method called on an incompatible receiver"))
    }

    fn this_object<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<Value<'gc>, Error> {
        match this {
            Value::Object(_) => Ok(this),
            _ => Err(a.throw(ErrorKind::Type, "RegExp.prototype method called on a non-object")),
        }
    }

    fn data(o: JsObject<'_>) -> (JsString<'_>, JsString<'_>, Gc<'_, Static<Box<dyn core::any::Any>>>) {
        let ObjectKind::RegExp(d) = &o.borrow().kind else { unreachable!("checked to be a regexp") };
        (d.source, d.flags, d.compiled)
    }

    /// Find the first match starting at `start` (by code point with `u` / `v`, otherwise by code unit).
    fn find(o: JsObject<'_>, units: &[u16], start: usize) -> Option<Found> {
        let (_, flags, compiled) = Self::data(o);
        let re = &compiled.0.downcast_ref::<Compiled>().expect("a compiled regex").regex;
        let m = if flags.contains('u') || flags.contains('v') { re.find_from_utf16(units, start).next() } else { re.find_from_ucs2(units, start).next() }?;
        let named = m.named_groups().map(|(n, r)| (String::from(n), r)).collect();
        Some(Found { range: m.range(), groups: m.captures, named })
    }

    /// All-ASCII strings are matched directly as UTF-8 (byte index = code unit index), skipping the conversion of the
    /// whole string to UTF-16.
    fn find_ascii(o: JsObject<'_>, s: &str, start: usize) -> Option<Found> {
        let (_, _, compiled) = Self::data(o);
        let c = compiled.0.downcast_ref::<Compiled>().expect("a compiled regex");
        if let Some(atom) = &c.atom {
            let at = start + s[start..].find(&**atom)?;
            return Some(Found { range: at..at + atom.len(), groups: Vec::new(), named: Vec::new() });
        }
        let m = c.regex.find_from(s, start).next()?;
        let named = m.named_groups().map(|(n, r)| (String::from(n), r)).collect();
        Some(Found { range: m.range(), groups: m.captures, named })
    }

    fn text<'gc>(a: &Agent<'gc, '_>, units: &[u16], r: Option<core::ops::Range<usize>>) -> Value<'gc> {
        r.map_or(Value::Undefined, |r| a.string(&String::from_utf16_lossy(&units[r])).into())
    }

    fn index<'gc>(i: usize) -> Value<'gc> {
        Value::Number(f64::from(u32::try_from(i).unwrap_or(u32::MAX)))
    }

    fn pair<'gc>(a: &Agent<'gc, '_>, r: Option<&core::ops::Range<usize>>) -> Value<'gc> {
        r.map_or(Value::Undefined, |r| a.array(alloc::vec![Self::index(r.start), Self::index(r.end)]).into())
    }

    /// RegExpBuiltinExec.
    fn builtin_exec<'gc>(a: &mut Agent<'gc, '_>, o: JsObject<'gc>, s: JsString<'gc>) -> Result<Value<'gc>, Error> {
        let li = a.get_named(o.into(), "lastIndex")?;
        let last = a.to_length(li)?;
        let flags = Self::data(o).1;
        let (global, sticky, indices) = (flags.contains('g'), flags.contains('y'), flags.contains('d'));
        // All ASCII: no UTF-16 conversion; match as UTF-8 and slice by byte
        let ascii = s.is_ascii();
        let units: Vec<u16> = if ascii { Vec::new() } else { s.encode_utf16().collect() };
        let text = |a: &Agent<'gc, '_>, r: Option<core::ops::Range<usize>>| if ascii { r.map_or(Value::Undefined, |r| a.string(&s[r]).into()) } else { Self::text(a, &units, r) };
        #[expect(clippy::cast_precision_loss, reason = "string length is far below 2^53")]
        let len = (if ascii { s.len() } else { units.len() }) as f64;
        let last = if global || sticky { last } else { 0.0 };
        if last > len {
            if global || sticky {
                a.set_named(o.into(), "lastIndex", 0.0.into())?;
            }
            return Ok(Value::Null);
        }
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "checked not to exceed the length")]
        let start = last as usize;
        let found = if ascii { Self::find_ascii(o, &s, start) } else { Self::find(o, &units, start) };
        let Some(m) = found.filter(|m| !sticky || m.range.start == start) else {
            if global || sticky {
                a.set_named(o.into(), "lastIndex", 0.0.into())?;
            }
            return Ok(Value::Null);
        };
        if global || sticky {
            a.set_named(o.into(), "lastIndex", Self::index(m.range.end))?;
        }
        let arr = a.array(Vec::new());
        a.create_data_property_or_throw(arr, Key::Str("index"), Self::index(m.range.start))?;
        a.create_data_property_or_throw(arr, Key::Str("input"), s.into())?;
        let whole = text(a, Some(m.range.clone()));
        a.create_index(arr, 0.0, whole)?;
        let groups = if m.named.is_empty() { None } else { Some(ObjectData::default().alloc(a.mc)) };
        a.create_data_property_or_throw(arr, Key::Str("groups"), groups.map_or(Value::Undefined, Value::Object))?;
        for (i, g) in (1..).zip(&m.groups) {
            let v = text(a, g.clone());
            a.create_index(arr, f64::from(i), v)?;
        }
        if let Some(g) = groups {
            for (n, r) in &m.named {
                let v = text(a, r.clone());
                a.create_data_property_or_throw(g, Key::Str(n), v)?;
            }
        }
        if indices {
            let mut pairs = alloc::vec![Self::pair(a, Some(&m.range))];
            pairs.extend(m.groups.iter().map(|g| Self::pair(a, g.as_ref())));
            let ind = a.array(pairs);
            let ig = if m.named.is_empty() { Value::Undefined } else { ObjectData::default().alloc(a.mc).into() };
            if let Value::Object(igo) = ig {
                for (n, r) in &m.named {
                    let v = Self::pair(a, r.as_ref());
                    a.create_data_property_or_throw(igo, Key::Str(n), v)?;
                }
            }
            a.create_data_property_or_throw(ind, Key::Str("groups"), ig)?;
            a.create_data_property_or_throw(arr, Key::Str("indices"), ind.into())?;
        }
        Ok(arr.into())
    }

    /// The RegExp object and `RegExp.prototype` are both pristine: the object's only own property is a writable numeric
    /// `lastIndex`, its prototype is `%RegExp.prototype%`, and `exec` / `flags` / the flag getters on the prototype are
    /// still the ones installed at setup. Then walking through the spec protocol (reading flags, reading/writing
    /// lastIndex, calling exec, reading the result array) is unobservable, so we can match directly. Returns the object
    /// and the value of `lastIndex`.
    fn pristine<'gc>(a: &Agent<'gc, '_>, r: Value<'gc>) -> Option<(JsObject<'gc>, f64)> {
        let Value::Object(o) = r else { return None };
        let d = o.borrow();
        let own_ok = matches!(d.kind, ObjectKind::RegExp(_))
            && d.proto.is_some_and(|p| Gc::ptr_eq(p, a.realm.regexp_proto))
            && d.symbols.is_empty()
            && d.props.len() == 1
            && &**d.props.key(0) == "lastIndex"
            && d.props.attrs(0).writable();
        if !own_ok {
            return None;
        }
        let Slot::Data(Value::Number(last)) = d.props.slot(0) else { return None };
        drop(d);
        let pristine = a.realm.regexp_pristine.borrow();
        let p = pristine.as_ref()?;
        let proto = a.realm.regexp_proto.borrow();
        let same = |x: Slot<'gc>, y: Slot<'gc>| match (x, y) {
            (Slot::Data(Value::Object(x)), Slot::Data(Value::Object(y))) | (Slot::Accessor { get: Some(x), .. }, Slot::Accessor { get: Some(y), .. }) => Gc::ptr_eq(x, y),
            _ => false,
        };
        let intact = proto.props.shape().is_some_and(|s| alloc::rc::Rc::ptr_eq(s, &p.shape)) && p.slots.iter().all(|&(i, orig)| same(proto.props.slot(i), orig));
        intact.then_some((o, last))
    }

    /// Set `lastIndex` of a pristine regex ([`pristine`](Self::pristine) checked it: slot 0, writable)
    fn set_last<'gc>(a: &Agent<'gc, '_>, o: JsObject<'gc>, i: usize) {
        o.borrow_mut(a.mc).set_at(0, Self::index(i));
    }

    /// Search a pristine regex from `start` (by byte if all ASCII, otherwise by UTF-16 code unit)
    fn find_in(o: JsObject<'_>, s: &str, units: Units<'_>, start: usize) -> Option<Found> {
        match units {
            Units::Ascii(_) => Self::find_ascii(o, s, start),
            Units::Wide(w) => Self::find(o, w, start),
        }
    }

    /// RegExpBuiltinExec for a pristine regex without building a result array: reads/writes `lastIndex` according to
    /// `g` / `y`, returns the match
    fn quick_exec<'gc>(a: &mut Agent<'gc, '_>, o: JsObject<'gc>, last: f64, s: &str, units: Units<'_>) -> Result<Option<Found>, Error> {
        let flags = Self::data(o).1;
        let (global, sticky) = (flags.contains('g'), flags.contains('y'));
        let len = units.len();
        let last = if global || sticky { a.to_length(last.into())? } else { 0.0 };
        #[expect(clippy::cast_precision_loss, reason = "string length is far below 2^53")]
        let fits = last <= len as f64;
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "checked not to exceed the length")]
        let start = last as usize;
        let found = if fits { Self::find_in(o, s, units, start).filter(|m| !sticky || m.range.start == start) } else { None };
        if global || sticky {
            Self::set_last(a, o, found.as_ref().map_or(0, |m| m.range.end));
        }
        Ok(found)
    }

    /// `[Symbol.replace]` for a pristine regex with a string replacement. Returns `None` when the template contains
    /// `$<` and the regex may have named groups (take the slow path; no side effects so far)
    fn quick_replace<'gc>(a: &mut Agent<'gc, '_>, o: JsObject<'gc>, last: f64, s: JsString<'gc>, with: JsString<'gc>) -> Result<Option<Value<'gc>>, Error> {
        let (source, flags, _) = Self::data(o);
        if with.contains("$<") && source.contains("(?<") {
            return Ok(None);
        }
        let (global, sticky, unicode) = (flags.contains('g'), flags.contains('y'), flags.contains('u') || flags.contains('v'));
        let units = s.units();
        let mut found = Vec::new();
        if global {
            let mut start = 0;
            while start <= units.len() {
                let Some(m) = Self::find_in(o, &s, units, start).filter(|m| !sticky || m.range.start == start) else { break };
                let next = match (m.range.is_empty(), units) {
                    (false, _) => m.range.end,
                    (true, Units::Ascii(_)) => m.range.end + 1,
                    #[expect(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "index is far below 2^53")]
                    (true, Units::Wide(w)) => Self::advance(w, m.range.end as f64, unicode) as usize,
                };
                found.push(m);
                start = next;
            }
            Self::set_last(a, o, 0);
        } else {
            found.extend(Self::quick_exec(a, o, last, &s, units)?);
        }
        // All ASCII and no `$` in the template: join directly as UTF-8 (byte index = code unit index)
        if let Units::Ascii(_) = units
            && !with.contains('$')
        {
            let mut out = String::with_capacity(s.len());
            let mut next = 0;
            for m in found {
                out.push_str(&s[next..m.range.start]);
                out.push_str(&with);
                next = m.range.end;
            }
            out.push_str(&s[next..]);
            return Ok(Some(a.string(&out).into()));
        }
        let units = units.to_vec();
        let template: Vec<u16> = with.encode_utf16().collect();
        let dollar = template.contains(&u16::from(b'$'));
        let mut out: Vec<u16> = Vec::with_capacity(units.len());
        let mut next = 0;
        for m in found {
            let (start, end) = (m.range.start, m.range.end);
            out.extend_from_slice(&units[next..start]);
            if dollar {
                let captures: Vec<Value<'gc>> = m.groups.iter().map(|g| Self::text(a, &units, g.clone())).collect();
                out.extend(Self::substitution(a, &units[start..end], &units, start, &captures, Value::Undefined, &template)?);
            } else {
                out.extend_from_slice(&template);
            }
            next = end;
        }
        out.extend_from_slice(&units[next..]);
        Ok(Some(a.string(&String::from_utf16_lossy(&out)).into()))
    }

    /// RegExpExec: if `exec` has been replaced, call it (the result must be an object or null).
    fn regexp_exec<'gc>(a: &mut Agent<'gc, '_>, r: Value<'gc>, s: JsString<'gc>) -> Result<Value<'gc>, Error> {
        let exec = a.get_named(r, "exec")?;
        if a.is_callable(exec) {
            let result = a.call(exec, r, &[s.into()])?;
            if !matches!(result, Value::Object(_) | Value::Null) {
                return Err(a.throw(ErrorKind::Type, "exec result must be an object or null"));
            }
            return Ok(result);
        }
        let o = Self::this(a, r)?;
        Self::builtin_exec(a, o, s)
    }

    fn exec<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::this(a, this)?;
        let s = a.to_string(args.at(0))?;
        Self::builtin_exec(a, o, s)
    }

    fn test<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let r = Self::this_object(a, this)?;
        let s = a.to_string(args.at(0))?;
        if let Some((o, last)) = Self::pristine(a, r) {
            return Ok(Self::quick_exec(a, o, last, &s, s.units())?.is_some().into());
        }
        Ok((Self::regexp_exec(a, r, s)? != Value::Null).into())
    }

    fn to_string<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let r = Self::this_object(a, this)?;
        let p = a.get_named(r, "source")?;
        let p = a.to_string(p)?;
        let f = a.get_named(r, "flags")?;
        let f = a.to_string(f)?;
        Ok(a.string(&alloc::format!("/{}/{}", &**p, &**f)).into())
    }

    /// EscapeRegExpPattern: escape `/` and line terminators; the empty string becomes `(?:)`.
    fn escape_pattern(source: &str) -> String {
        if source.is_empty() {
            return String::from("(?:)");
        }
        let mut out = String::new();
        let mut escaped = false;
        let mut in_class = false;
        for c in source.chars() {
            match c {
                '/' if !escaped && !in_class => out.push_str("\\/"),
                '\n' => out.push_str(if escaped { "n" } else { "\\n" }),
                '\r' => out.push_str(if escaped { "r" } else { "\\r" }),
                '\u{2028}' => out.push_str(if escaped { "u2028" } else { "\\u2028" }),
                '\u{2029}' => out.push_str(if escaped { "u2029" } else { "\\u2029" }),
                _ => out.push(c),
            }
            if !escaped {
                match c {
                    '[' => in_class = true,
                    ']' => in_class = false,
                    _ => {}
                }
            }
            escaped = c == '\\' && !escaped;
        }
        out
    }

    fn source<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        if let Value::Object(o) = this
            && Gc::ptr_eq(o, a.realm.regexp_proto)
        {
            return Ok(a.string("(?:)").into());
        }
        let o = Self::this(a, this)?;
        let s = Self::escape_pattern(&Self::data(o).0);
        Ok(a.string(&s).into())
    }

    /// `flags`: read each flag property in spec order (generic; does not require a RegExp object).
    fn flags<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let r = Self::this_object(a, this)?;
        let mut out = String::new();
        for (c, name) in FLAG_PROPERTIES {
            let v = a.get_named(r, name)?;
            if a.to_boolean(v) {
                out.push(c);
            }
        }
        Ok(a.string(&out).into())
    }

    /// RegExpHasFlag: `RegExp.prototype` itself gives `undefined`.
    fn flag<'gc, const C: char>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let Value::Object(o) = this else { return Err(a.throw(ErrorKind::Type, "RegExp flag getter called on a non-object")) };
        match Self::regexp(this) {
            Some(r) => Ok(Self::data(r).1.contains(C).into()),
            None if Gc::ptr_eq(o, a.realm.regexp_proto) => Ok(Value::Undefined),
            None => Err(a.throw(ErrorKind::Type, "RegExp flag getter called on an incompatible receiver")),
        }
    }

    fn get_flags<'gc>(a: &mut Agent<'gc, '_>, r: Value<'gc>) -> Result<String, Error> {
        let f = a.get_named(r, "flags")?;
        Ok(String::from(&**a.to_string(f)?))
    }

    /// AdvanceStringIndex.
    fn advance(units: &[u16], index: f64, unicode: bool) -> f64 {
        if !unicode {
            return index + 1.0;
        }
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "index is non-negative")]
        let i = index as usize;
        let lead = units.get(i).copied();
        let trail = units.get(i + 1).copied();
        match (lead, trail) {
            (Some(l), Some(t)) if (0xd800..0xdc00).contains(&l) && (0xdc00..0xe000).contains(&t) => index + 2.0,
            _ => index + 1.0,
        }
    }

    /// Advance `lastIndex` after an empty match.
    fn step_last_index<'gc>(a: &mut Agent<'gc, '_>, r: Value<'gc>, units: &[u16], unicode: bool) -> Result<(), Error> {
        let li = a.get_named(r, "lastIndex")?;
        let this_index = a.to_length(li)?;
        a.set_named(r, "lastIndex", Self::advance(units, this_index, unicode).into())
    }

    fn match_symbol<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let r = Self::this_object(a, this)?;
        let s = a.to_string(args.at(0))?;
        let flags = Self::get_flags(a, r)?;
        if !flags.contains('g') {
            return Self::regexp_exec(a, r, s);
        }
        let unicode = flags.contains('u') || flags.contains('v');
        a.set_named(r, "lastIndex", 0.0.into())?;
        let units: Vec<u16> = s.encode_utf16().collect();
        let arr = a.array(Vec::new());
        let mut n = 0.0;
        loop {
            let result = Self::regexp_exec(a, r, s)?;
            if result == Value::Null {
                return Ok(if n == 0.0 { Value::Null } else { arr.into() });
            }
            let m = a.get_index(result, 0.0)?;
            let m = a.to_string(m)?;
            a.create_index(arr, n, m.into())?;
            if m.is_empty() {
                Self::step_last_index(a, r, &units, unicode)?;
            }
            n += 1.0;
        }
    }

    fn match_all_symbol<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let r = Self::this_object(a, this)?;
        let s = a.to_string(args.at(0))?;
        let default = a.realm.global.borrow().own("RegExp").unwrap_or_default();
        let ctor = Buffers::species_constructor(a, r, default)?;
        let flags = Self::get_flags(a, r)?;
        let f = a.string(&flags);
        let matcher = a.construct(ctor, &[r, f.into()], ctor)?;
        let li = a.get_named(r, "lastIndex")?;
        let last = a.to_length(li)?;
        a.set_named(matcher, "lastIndex", last.into())?;
        let Value::Object(matcher) = matcher else { return Err(a.throw(ErrorKind::Type, "species constructor returned a non-object")) };
        let kind = ObjectKind::RegExpStringIterator { matcher, string: s, global: flags.contains('g'), unicode: flags.contains('u') || flags.contains('v'), done: false };
        Ok(ObjectData { kind, proto: Some(a.realm.regexp_string_iterator_proto), ..ObjectData::default() }.alloc(a.mc).into())
    }

    fn string_iterator_next<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let state = match this {
            Value::Object(o) => match o.borrow().kind {
                ObjectKind::RegExpStringIterator { matcher, string, global, unicode, done } => Some((o, matcher, string, global, unicode, done)),
                _ => None,
            },
            _ => None,
        };
        let Some((o, matcher, s, global, unicode, done)) = state else {
            return Err(a.throw(ErrorKind::Type, "%RegExpStringIterator%.next called on an incompatible receiver"));
        };
        let finish = |a: &mut Agent<'gc, '_>| {
            if let ObjectKind::RegExpStringIterator { done, .. } = &mut o.borrow_mut(a.mc).kind {
                *done = true;
            }
        };
        if done {
            return Ok(a.iter_result(Value::Undefined, true).into());
        }
        let m = Self::regexp_exec(a, matcher.into(), s)?;
        if m == Value::Null {
            finish(a);
            return Ok(a.iter_result(Value::Undefined, true).into());
        }
        if global {
            let text = a.get_index(m, 0.0)?;
            if a.to_string(text)?.is_empty() {
                let units: Vec<u16> = s.encode_utf16().collect();
                Self::step_last_index(a, matcher.into(), &units, unicode)?;
            }
        } else {
            finish(a);
        }
        Ok(a.iter_result(m, false).into())
    }

    /// GetSubstitution: `$$` / `$&` / `` $` `` / `$'` / `$n` / `$nn` / `$<name>` in a replacement template.
    fn substitution<'gc>(a: &mut Agent<'gc, '_>, matched: &[u16], units: &[u16], position: usize, captures: &[Value<'gc>], named: Value<'gc>, template: &[u16]) -> Result<Vec<u16>, Error> {
        let dollar = u16::from(b'$');
        let digit = |c: u16| char::from_u32(u32::from(c)).and_then(|c| c.to_digit(10));
        let tail = (position + matched.len()).min(units.len());
        let mut out = Vec::new();
        let mut i = 0;
        let m = captures.len();
        while i < template.len() {
            let c = template[i];
            let Some(n) = template.get(i + 1).copied().filter(|_| c == dollar) else {
                out.push(c);
                i += 1;
                continue;
            };
            match char::from_u32(u32::from(n)) {
                Some('$') => {
                    out.push(dollar);
                    i += 2;
                }
                Some('&') => {
                    out.extend_from_slice(matched);
                    i += 2;
                }
                Some('`') => {
                    out.extend_from_slice(&units[..position.min(units.len())]);
                    i += 2;
                }
                Some('\'') => {
                    out.extend_from_slice(&units[tail..]);
                    i += 2;
                }
                Some('<') if named != Value::Undefined => match template[i + 2..].iter().position(|&c| c == u16::from(b'>')) {
                    Some(len) => {
                        let name = String::from_utf16_lossy(&template[i + 2..i + 2 + len]);
                        let v = a.get_named(named, &name)?;
                        if v != Value::Undefined {
                            out.extend(a.to_string(v)?.encode_utf16());
                        }
                        i += len + 3;
                    }
                    None => {
                        out.push(c);
                        i += 1;
                    }
                },
                _ => {
                    let capture = |k: usize| -> Option<Value<'gc>> { captures.get(k.checked_sub(1)?).copied() };
                    let d1 = digit(n);
                    let two = d1.and_then(|d1| template.get(i + 2).copied().and_then(digit).map(|d2| (d1 * 10 + d2) as usize)).filter(|&k| k >= 1 && k <= m);
                    let one = d1.map(|d| d as usize).filter(|&k| k >= 1 && k <= m);
                    let (k, used) = match (two, one) {
                        (Some(k), _) => (k, 3),
                        (None, Some(k)) => (k, 2),
                        _ => {
                            out.push(c);
                            i += 1;
                            continue;
                        }
                    };
                    if let Some(v) = capture(k)
                        && v != Value::Undefined
                    {
                        out.extend(a.to_string(v)?.encode_utf16());
                    }
                    i += used;
                }
            }
        }
        Ok(out)
    }

    fn replace_symbol<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let r = Self::this_object(a, this)?;
        let s = a.to_string(args.at(0))?;
        let mut with = args.at(1);
        let functional = a.is_callable(with);
        if !functional {
            let w = a.to_string(with)?;
            if let Some((o, last)) = Self::pristine(a, r)
                && let Some(v) = Self::quick_replace(a, o, last, s, w)?
            {
                return Ok(v);
            }
            with = w.into();
        }
        let units: Vec<u16> = s.encode_utf16().collect();
        let flags = Self::get_flags(a, r)?;
        let global = flags.contains('g');
        let unicode = flags.contains('u') || flags.contains('v');
        if global {
            a.set_named(r, "lastIndex", 0.0.into())?;
        }
        let mut results = Vec::new();
        loop {
            let result = Self::regexp_exec(a, r, s)?;
            if result == Value::Null {
                break;
            }
            results.push(result);
            if !global {
                break;
            }
            let m = a.get_index(result, 0.0)?;
            if a.to_string(m)?.is_empty() {
                Self::step_last_index(a, r, &units, unicode)?;
            }
        }
        let mut out: Vec<u16> = Vec::new();
        let mut next = 0;
        for result in results {
            let len = a.length_of_array_like(result)?;
            let n_captures = (len - 1.0).max(0.0);
            let matched = a.get_index(result, 0.0)?;
            let matched: Vec<u16> = a.to_string(matched)?.encode_utf16().collect();
            let pos = a.get_named(result, "index")?;
            let pos = a.to_integer_or_infinity(pos)?;
            #[expect(clippy::cast_precision_loss, reason = "string length is far below 2^53")]
            let pos = pos.clamp(0.0, units.len() as f64);
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to [0, length]")]
            let position = pos as usize;
            let mut captures = Vec::new();
            let mut k = 1.0;
            while k <= n_captures {
                let c = a.get_index(result, k)?;
                captures.push(if c == Value::Undefined { c } else { a.to_string(c)?.into() });
                k += 1.0;
            }
            let named = a.get_named(result, "groups")?;
            let replacement: Vec<u16> = if functional {
                let mut call_args = alloc::vec![a.string(&String::from_utf16_lossy(&matched)).into()];
                call_args.extend(captures.iter().copied());
                call_args.push(pos.into());
                call_args.push(s.into());
                if named != Value::Undefined {
                    call_args.push(named);
                }
                let v = a.call(with, Value::Undefined, &call_args)?;
                a.to_string(v)?.encode_utf16().collect()
            } else {
                let named = if named == Value::Undefined { named } else { a.to_object(named)?.into() };
                let template: Vec<u16> = a.to_string(with)?.encode_utf16().collect();
                Self::substitution(a, &matched, &units, position, &captures, named, &template)?
            };
            if position >= next {
                out.extend_from_slice(&units[next..position]);
                out.extend(replacement);
                next = (position + matched.len()).min(units.len());
            }
        }
        if next < units.len() {
            out.extend_from_slice(&units[next..]);
        }
        Ok(a.string(&String::from_utf16_lossy(&out)).into())
    }

    fn search_symbol<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let r = Self::this_object(a, this)?;
        let s = a.to_string(args.at(0))?;
        let previous = a.get_named(r, "lastIndex")?;
        if !previous.same_value(0.0.into()) {
            a.set_named(r, "lastIndex", 0.0.into())?;
        }
        let result = Self::regexp_exec(a, r, s)?;
        let current = a.get_named(r, "lastIndex")?;
        if !current.same_value(previous) {
            a.set_named(r, "lastIndex", previous)?;
        }
        if result == Value::Null {
            return Ok((-1.0).into());
        }
        a.get_named(result, "index")
    }

    fn split_symbol<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let r = Self::this_object(a, this)?;
        let s = a.to_string(args.at(0))?;
        let default = a.realm.global.borrow().own("RegExp").unwrap_or_default();
        let ctor = Buffers::species_constructor(a, r, default)?;
        let flags = Self::get_flags(a, r)?;
        let unicode = flags.contains('u') || flags.contains('v');
        let new_flags = if flags.contains('y') { flags } else { alloc::format!("{flags}y") };
        let nf = a.string(&new_flags);
        let splitter = a.construct(ctor, &[r, nf.into()], ctor)?;
        let arr = a.array(Vec::new());
        let lim = match args.at(1) {
            Value::Undefined => MAX_SPLIT,
            v => f64::from(a.to_uint32(v)?),
        };
        if lim == 0.0 {
            return Ok(arr.into());
        }
        let units: Vec<u16> = s.encode_utf16().collect();
        #[expect(clippy::cast_precision_loss, reason = "string length is far below 2^53")]
        let size = units.len() as f64;
        if units.is_empty() {
            if Self::regexp_exec(a, splitter, s)? == Value::Null {
                a.create_index(arr, 0.0, s.into())?;
            }
            return Ok(arr.into());
        }
        let slice = |a: &Agent<'gc, '_>, from: f64, to: f64| {
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "index is in [0, length]")]
            let (f, t) = (from as usize, to as usize);
            Self::text(a, &units, Some(f..t.max(f)))
        };
        let mut n = 0.0;
        let (mut p, mut q) = (0.0, 0.0);
        while q < size {
            a.set_named(splitter, "lastIndex", q.into())?;
            let z = Self::regexp_exec(a, splitter, s)?;
            if z == Value::Null {
                q = Self::advance(&units, q, unicode);
                continue;
            }
            let e = a.get_named(splitter, "lastIndex")?;
            let e = a.to_length(e)?.min(size);
            if e == p {
                q = Self::advance(&units, q, unicode);
                continue;
            }
            let t = slice(a, p, q);
            a.create_index(arr, n, t)?;
            n += 1.0;
            if n == lim {
                return Ok(arr.into());
            }
            p = e;
            let count = a.length_of_array_like(z)?;
            let mut i = 1.0;
            while i < count {
                let c = a.get_index(z, i)?;
                a.create_index(arr, n, c)?;
                n += 1.0;
                if n == lim {
                    return Ok(arr.into());
                }
                i += 1.0;
            }
            q = p;
        }
        let t = slice(a, p, size);
        a.create_index(arr, n, t)?;
        Ok(arr.into())
    }

    fn species<'gc>(_: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(this)
    }

    /// `RegExp.escape(s)`.
    fn escape<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let Value::String(s) = args.at(0) else { return Err(a.throw(ErrorKind::Type, "RegExp.escape requires a string")) };
        let mut out = String::new();
        for (i, c) in s.chars().enumerate() {
            match c {
                _ if i == 0 && c.is_ascii_alphanumeric() => _ = write!(out, "\\x{:02x}", u32::from(c)),
                '^' | '$' | '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '/' => {
                    out.push('\\');
                    out.push(c);
                }
                ',' | '-' | '=' | '<' | '>' | '#' | '&' | '!' | '%' | ':' | ';' | '@' | '~' | '\'' | '`' | '"' => _ = write!(out, "\\x{:02x}", u32::from(c)),
                '\t' => out.push_str("\\t"),
                '\n' => out.push_str("\\n"),
                '\u{b}' => out.push_str("\\v"),
                '\u{c}' => out.push_str("\\f"),
                '\r' => out.push_str("\\r"),
                ' ' => out.push_str("\\x20"),
                _ if Strings::is_space(c) => {
                    for u in c.encode_utf16(&mut [0; 2]) {
                        let _ = write!(out, "\\u{:04x}", *u);
                    }
                }
                _ => out.push(c),
            }
        }
        Ok(a.string(&out).into())
    }

    // ---- String methods: dispatched through the Symbol protocol ----

    fn subject<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>) -> Result<Value<'gc>, Error> {
        if this.is_nullish() {
            return Err(a.throw(ErrorKind::Type, "String.prototype method called on null or undefined"));
        }
        Ok(this)
    }

    /// GetMethod(v, sym): `None` if absent; throws TypeError if not callable.
    fn method_of<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>, sym: r8framework::JsSymbol<'gc>) -> Result<Option<Value<'gc>>, Error> {
        let f = a.get_symbol(v, sym, v)?;
        if f.is_nullish() {
            return Ok(None);
        }
        if !a.is_callable(f) {
            let t = a.describe(f);
            return Err(a.throw(ErrorKind::Type, &alloc::format!("{t} is not a function")));
        }
        Ok(Some(f))
    }

    /// match / search: delegate to the corresponding Symbol method if there is one, otherwise create a regex and call
    /// it.
    fn dispatch<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, arg: Value<'gc>, sym: r8framework::JsSymbol<'gc>, flags: Value<'gc>) -> Result<Value<'gc>, Error> {
        let o = Self::subject(a, this)?;
        if matches!(arg, Value::Object(_))
            && let Some(f) = Self::method_of(a, arg, sym)?
        {
            return a.call(f, arg, &[o]);
        }
        let s = a.to_string(o)?;
        let rx = Self::create(a, arg, flags)?;
        let f = a.get_symbol(rx.into(), sym, rx.into())?;
        a.call(f, rx.into(), &[s.into()])
    }

    fn string_match<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::dispatch(a, this, args.at(0), a.realm.symbols.match_, Value::Undefined)
    }

    fn search<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::dispatch(a, this, args.at(0), a.realm.symbols.search, Value::Undefined)
    }

    /// The regex passed to matchAll / replaceAll must have `g`.
    fn require_global<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>, method: &str) -> Result<(), Error> {
        if Self::is_regexp(a, v)? {
            let flags = a.get_named(v, "flags")?;
            if flags.is_nullish() {
                return Err(a.throw(ErrorKind::Type, "flags is null or undefined"));
            }
            if !a.to_string(flags)?.contains('g') {
                return Err(a.throw(ErrorKind::Type, &alloc::format!("String.prototype.{method} called with a non-global RegExp argument")));
            }
        }
        Ok(())
    }

    fn match_all<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let arg = args.at(0);
        Self::subject(a, this)?;
        if matches!(arg, Value::Object(_)) {
            Self::require_global(a, arg, "matchAll")?;
        }
        let g = a.string("g");
        Self::dispatch(a, this, arg, a.realm.symbols.match_all, g.into())
    }

    fn replace_impl<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>], all: bool) -> Result<Value<'gc>, Error> {
        let o = Self::subject(a, this)?;
        let (search, with) = (args.at(0), args.at(1));
        if matches!(search, Value::Object(_)) {
            if all {
                Self::require_global(a, search, "replaceAll")?;
            }
            if let Some(f) = Self::method_of(a, search, a.realm.symbols.replace)? {
                return a.call(f, search, &[o, with]);
            }
        }
        let s = a.to_string(o)?;
        let units: Vec<u16> = s.encode_utf16().collect();
        let needle: Vec<u16> = a.to_string(search)?.encode_utf16().collect();
        let functional = a.is_callable(with);
        let template: Vec<u16> = if functional { Vec::new() } else { a.to_string(with)?.encode_utf16().collect() };
        let mut positions = Vec::new();
        let mut at = Strings::find(&units, &needle, 0);
        while let Some(p) = at {
            positions.push(p);
            if !all {
                break;
            }
            at = Strings::find(&units, &needle, p + needle.len().max(1));
        }
        let mut out = Vec::new();
        let mut end = 0;
        for p in positions {
            out.extend_from_slice(&units[end..p]);
            let replacement: Vec<u16> = if functional {
                let n = a.string(&String::from_utf16_lossy(&needle));
                let v = a.call(with, Value::Undefined, &[n.into(), Self::index(p), s.into()])?;
                a.to_string(v)?.encode_utf16().collect()
            } else {
                Self::substitution(a, &needle, &units, p, &[], Value::Undefined, &template)?
            };
            out.extend(replacement);
            end = p + needle.len();
        }
        out.extend_from_slice(&units[end.min(units.len())..]);
        Ok(a.string(&String::from_utf16_lossy(&out)).into())
    }

    fn replace<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::replace_impl(a, this, args, false)
    }

    fn replace_all<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::replace_impl(a, this, args, true)
    }

    fn split<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::subject(a, this)?;
        let sep = args.at(0);
        if matches!(sep, Value::Object(_))
            && let Some(f) = Self::method_of(a, sep, a.realm.symbols.split)?
        {
            return a.call(f, sep, &[o, args.at(1)]);
        }
        Strings::split(a, this, args)
    }
}

impl<'gc> Installer<'_, 'gc> {
    pub(crate) fn regexp(&mut self) {
        let r = self.realm;
        let proto = r.regexp_proto;
        let ctor = self.constructor("RegExp", Regexps::construct, proto, 2);
        self.method(ctor, "escape", Regexps::escape, 1);
        let species = r.named_native(self.mc, Regexps::species, "get [Symbol.species]", 0);
        ctor.borrow_mut(self.mc).define_symbol(r.symbols.species, Property { slot: Slot::Accessor { get: Some(species), set: None }, enumerable: false, writable: false, configurable: true });
        // Internal constructor used by regex literals
        r.regexp_ctor.borrow_mut(self.mc).kind = ObjectKind::Native(r8framework::Native::function(Regexps::construct));
        self.method(proto, "exec", Regexps::exec, 1);
        self.method(proto, "test", Regexps::test, 1);
        self.method(proto, "toString", Regexps::to_string, 0);
        self.method(proto, "compile", Regexps::compile_method, 2);
        self.getter(proto, "source", Regexps::source);
        self.getter(proto, "flags", Regexps::flags);
        let flags: [(&str, r8framework::NativeFn); 8] = [
            ("hasIndices", Regexps::flag::<'d'>),
            ("global", Regexps::flag::<'g'>),
            ("ignoreCase", Regexps::flag::<'i'>),
            ("multiline", Regexps::flag::<'m'>),
            ("dotAll", Regexps::flag::<'s'>),
            ("unicode", Regexps::flag::<'u'>),
            ("unicodeSets", Regexps::flag::<'v'>),
            ("sticky", Regexps::flag::<'y'>),
        ];
        for (name, f) in flags {
            self.getter(proto, name, f);
        }
        // Record the state at setup: slots for exec / flags / the flag getters
        let pristine = {
            let d = proto.borrow();
            let slots = ["exec", "flags"].into_iter().chain(FLAG_PROPERTIES.map(|(_, n)| n)).filter_map(|n| d.props.position(n).map(|i| (i, d.props.slot(i)))).collect();
            d.props.shape().map(|shape| r8framework::RegExpPristine { shape: alloc::rc::Rc::clone(shape), slots })
        };
        *r.regexp_pristine.borrow_mut(self.mc) = pristine;
        let symbols: [(r8framework::JsSymbol<'gc>, r8framework::NativeFn, u32); 5] = [
            (r.symbols.match_, Regexps::match_symbol, 1),
            (r.symbols.match_all, Regexps::match_all_symbol, 1),
            (r.symbols.replace, Regexps::replace_symbol, 2),
            (r.symbols.search, Regexps::search_symbol, 1),
            (r.symbols.split, Regexps::split_symbol, 2),
        ];
        for (sym, f, n) in symbols {
            r.symbol_method(self.mc, proto, sym, f, n);
        }
        self.method(r.regexp_string_iterator_proto, "next", Regexps::string_iterator_next, 0);
        let s = r.string_proto;
        self.method(s, "match", Regexps::string_match, 1);
        self.method(s, "matchAll", Regexps::match_all, 1);
        self.method(s, "search", Regexps::search, 1);
        self.method(s, "replace", Regexps::replace, 2);
        self.method(s, "replaceAll", Regexps::replace_all, 2);
        self.method(s, "split", Regexps::split, 2);
    }
}
