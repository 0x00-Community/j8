use j8framework::ValueKind;
use alloc::boxed::Box;
use core::fmt::Write;
use alloc::string::String;
use alloc::vec::Vec;

use j8framework::{Gc, Static};
use j8framework::{Agent, Error, ErrorKind, JsObject, JsString, Key, ObjectData, ObjectKind, Property, RegExpData, Slot, SmallStr, Units, Value};

use crate::buffer::Buffers;
use crate::string::Strings;
use crate::{Args, Installer};

const FLAGS: &str = "dgimsuvy";

const ENGINE_FLAGS: &str = "imsuv";

const FLAG_PROPERTIES: [(char, &str); 8] = [('d', "hasIndices"), ('g', "global"), ('i', "ignoreCase"), ('m', "multiline"), ('s', "dotAll"), ('u', "unicode"), ('v', "unicodeSets"), ('y', "sticky")];

const MAX_SPLIT: f64 = 4_294_967_295.0;

struct Found {
    range: core::ops::Range<usize>,
    groups: Vec<Option<core::ops::Range<usize>>>,
    named: Vec<(String, Option<core::ops::Range<usize>>)>,
}

struct Compiled {
    regex: regress::Regex,
    atom: Option<Box<str>>,
    dfa: Option<crate::dfa::Dfa>,
    flags: u16,
    named: bool,
}

impl Compiled {
    const SYNTAX: &'static str = "\\^$.|?*+()[]{}/";
    const GLOBAL: u16 = 1;
    const IGNORE_CASE: u16 = 1 << 1;
    const MULTILINE: u16 = 1 << 2;
    const STICKY: u16 = 1 << 3;
    const UNICODE: u16 = 1 << 4;
    const DOT_ALL: u16 = 1 << 5;
    const INDICES: u16 = 1 << 7;
    const UNICODE_SETS: u16 = 1 << 8;

    fn new(regex: regress::Regex, source: &str, flags: &str) -> Self {
        let bits = Self::bits(flags);
        let atom = (!source.is_empty() && !Self::on(bits, Self::IGNORE_CASE) && !source.contains(|c| Self::SYNTAX.contains(c))).then(|| source.into());
        let dfa = if atom.is_none() { crate::dfa::Dfa::new(source, flags) } else { None };
        Self { regex, atom, dfa, flags: bits, named: source.contains("(?<") }
    }

    fn bits(flags: &str) -> u16 {
        let mut bits = 0;
        for c in flags.chars() {
            bits |= match c {
                'g' => Self::GLOBAL,
                'i' => Self::IGNORE_CASE,
                'm' => Self::MULTILINE,
                'y' => Self::STICKY,
                'u' => Self::UNICODE,
                's' => Self::DOT_ALL,
                'd' => Self::INDICES,
                'v' => Self::UNICODE_SETS,
                _ => 0,
            };
        }
        bits
    }

    fn text(bits: u16) -> String {
        let mut out = String::new();
        for (c, bit) in [('d', Self::INDICES), ('g', Self::GLOBAL), ('i', Self::IGNORE_CASE), ('m', Self::MULTILINE), ('s', Self::DOT_ALL), ('u', Self::UNICODE), ('v', Self::UNICODE_SETS), ('y', Self::STICKY)] {
            if Self::on(bits, bit) {
                out.push(c);
            }
        }
        out
    }

    fn on(bits: u16, bit: u16) -> bool {
        bits & bit != 0
    }

    fn global(&self) -> bool {
        Self::on(self.flags, Self::GLOBAL)
    }

    fn sticky(&self) -> bool {
        Self::on(self.flags, Self::STICKY)
    }

    fn unicode(&self) -> bool {
        Self::on(self.flags, Self::UNICODE | Self::UNICODE_SETS)
    }

    fn indices(&self) -> bool {
        Self::on(self.flags, Self::INDICES)
    }

    fn has(&self, c: char) -> bool {
        let bit = match c {
            'd' => Self::INDICES,
            'g' => Self::GLOBAL,
            'i' => Self::IGNORE_CASE,
            'm' => Self::MULTILINE,
            's' => Self::DOT_ALL,
            'u' => Self::UNICODE,
            'v' => Self::UNICODE_SETS,
            'y' => Self::STICKY,
            _ => 0,
        };
        Self::on(self.flags, bit)
    }
}

pub(crate) struct Regexps;

impl Regexps {
    fn compile(a: &Agent<'_, '_>, source: &str, flags: &str) -> Result<regress::Regex, Error> {
        let bad = flags.chars().enumerate().any(|(i, c)| !FLAGS.contains(c) || flags[..i].contains(c)) || (flags.contains('u') && flags.contains('v'));
        if bad {
            return Err(a.throw(ErrorKind::Syntax, &alloc::format!("Invalid flags supplied to RegExp constructor '{flags}'")));
        }
        let engine: String = flags.chars().filter(|&c| ENGINE_FLAGS.contains(c)).collect();
        regress::Regex::with_flags(source, engine.as_str()).map_err(|e| a.throw(ErrorKind::Syntax, &alloc::format!("Invalid regular expression: /{source}/{flags}: {e}")))
    }

    fn compiled<'gc>(a: &Agent<'gc, '_>, source: &str, flags: &str) -> Result<Gc<'gc, Static<Box<dyn core::any::Any + Send + Sync>>>, Error> {
        Self::cached(a, source, flags).map(|c| c.compiled)
    }

    fn cached<'gc>(a: &Agent<'gc, '_>, source: &str, flags: &str) -> Result<j8framework::RegExpCompiled<'gc>, Error> {
        let slot = &a.realm.realm().get().regexp_cache[j8framework::RegExpCompiled::bucket(source, flags)];
        if let Some(c) = slot.get().filter(|c| **c.source == *source && **c.flags == *flags) {
            return Ok((*c).clone());
        }
        let regex = Compiled::new(Self::compile(a, source, flags)?, source, flags);
        let compiled = a.mc.alloc(Static(Box::new(regex) as Box<dyn core::any::Any + Send + Sync>));
        let entry = j8framework::RegExpCompiled { source: a.string(source), flags: a.string(flags), compiled };
        slot.set(a.mc.mutator(), Some(a.mc.alloc(entry.clone())));
        Ok(entry)
    }

    fn make<'gc>(a: &Agent<'gc, '_>, c: &j8framework::RegExpCompiled<'gc>) -> JsObject<'gc> {
        let kind = ObjectKind::RegExp(RegExpData { source: c.source, flags: c.flags, compiled: c.compiled, legacy: true });
        let o = ObjectData { kind, proto: Some(a.realm.regexp_proto), ..ObjectData::default() }.alloc(a.mc);
        Self::stamp(a, o, 0.0.into());
        o
    }

    fn stamp<'gc>(a: &Agent<'gc, '_>, o: JsObject<'gc>, v: Value<'gc>) {
        if let Some(p) = a.realm.regexp_pristine.get().as_ref() {
            o.fill(a.mc, p.last_index, core::iter::once(Slot::Data(v)));
            return;
        }
        o.borrow_mut(a.mc).define("lastIndex", Property { slot: Slot::Data(v), enumerable: false, writable: true, configurable: false });
    }

    fn literal_bucket(ptr: usize, len: usize, flags: u16) -> usize {
        let mixed = (ptr as u64 ^ (len as u64).rotate_left(32) ^ u64::from(flags).rotate_left(48)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        usize::try_from(mixed >> 40).unwrap_or(0) % j8framework::RegExpCompiled::BUCKETS
    }

    fn literal_load<'gc>(slot: &j8framework::paragc::Slot<'gc, j8framework::RegExpLiteralEntry<'gc>>, ptr: usize, len: usize, flags: u16, source: &str) -> Option<j8framework::RegExpCompiled<'gc>> {
        let e = slot.get()?;
        (e.ptr == ptr && e.len == len && e.flags == flags && **e.compiled.source == *source).then(|| e.compiled.clone())
    }

    fn literal_store<'gc>(a: &Agent<'gc, '_>, bucket: usize, entry: j8framework::RegExpLiteralEntry<'gc>) {
        let cache = &a.realm.realm().get().regexp_literals;
        let buddy = (bucket + 1) % j8framework::RegExpCompiled::BUCKETS;
        let key = |e: &j8framework::RegExpLiteralEntry<'gc>| e.ptr == entry.ptr && e.len == entry.len && e.flags == entry.flags;
        let at = if cache[bucket].get().is_none_or(|e| key(&e)) || cache[buddy].get().is_some_and(|e| !key(&e)) { bucket } else { buddy };
        cache[at].set(a.mc.mutator(), Some(a.mc.alloc(entry)));
    }

    pub(crate) fn literal<'gc>(a: &Agent<'gc, '_>, source: &str, flags: u16) -> Result<Value<'gc>, Error> {
        let ptr = source.as_ptr().addr();
        let len = source.len();
        let bucket = Self::literal_bucket(ptr, len, flags);
        let buddy = (bucket + 1) % j8framework::RegExpCompiled::BUCKETS;
        let hit = {
            let cache = &a.realm.realm().get().regexp_literals;
            Self::literal_load(&cache[bucket], ptr, len, flags, source).or_else(|| Self::literal_load(&cache[buddy], ptr, len, flags, source))
        };
        if let Some(c) = hit {
            return Ok(Self::make(a, &c).into());
        }
        let compiled = Self::cached(a, source, &Compiled::text(flags))?;
        Self::literal_store(a, bucket, j8framework::RegExpLiteralEntry { ptr, len, flags, compiled: compiled.clone() });
        Ok(Self::make(a, &compiled).into())
    }

    pub(crate) fn is_regexp<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<bool, Error> {
        if v.as_object().is_none() {
            return Ok(false);
        }
        let m = a.get_symbol(v, a.realm.symbols.match_, v)?;
        if m != Value::Undefined {
            return Ok(a.to_boolean(m));
        }
        Ok(Self::regexp(v).is_some())
    }

    fn is_regexp_ctor(v: Value<'_>) -> bool {
        matches!(v.kind(), ValueKind::Object(f) if matches!(f.borrow().kind, ObjectKind::Native(n) if core::ptr::fn_addr_eq(n.f, Self::construct as j8framework::NativeFn)))
    }

    fn initialize<'gc>(a: &mut Agent<'gc, '_>, o: JsObject<'gc>, pattern: Value<'gc>, flags: Value<'gc>) -> Result<(), Error> {
        let source = if pattern == Value::Undefined { a.string("") } else { a.to_string(pattern)? };
        let flags = if flags == Value::Undefined { a.string("") } else { a.to_string(flags)? };
        let compiled = Self::compiled(a, &source, &flags)?;
        let legacy = matches!(&o.borrow().kind, ObjectKind::RegExp(d) if d.legacy);
        o.borrow_mut(a.mc).kind = ObjectKind::RegExp(RegExpData { source, flags, compiled, legacy });
        a.set_named(o.into(), "lastIndex", 0.0.into())
    }

    fn alloc<'gc>(a: &mut Agent<'gc, '_>, new_target: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        let proto = a.proto_from_ctor(new_target, a.realm.regexp_proto)?;
        let legacy = new_target == Value::Undefined || Self::is_regexp_ctor(new_target);
        let empty = a.string("");
        let compiled = Self::compiled(a, "", "")?;
        let kind = ObjectKind::RegExp(RegExpData { source: empty, flags: empty, compiled, legacy });
        let o = ObjectData { kind, proto: Some(proto), ..ObjectData::default() }.alloc(a.mc);
        if Gc::ptr_eq(proto, a.realm.regexp_proto) {
            Self::stamp(a, o, Value::Undefined);
        } else {
            o.borrow_mut(a.mc).define("lastIndex", Property { slot: Slot::Data(Value::Undefined), enumerable: false, writable: true, configurable: false });
        }
        Ok(o)
    }

    fn create<'gc>(a: &mut Agent<'gc, '_>, pattern: Value<'gc>, flags: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        let o = Self::alloc(a, Value::Undefined)?;
        Self::initialize(a, o, pattern, flags)?;
        Ok(o)
    }

    fn construct<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let new_target = a.new_target;
        let (pattern, flags) = (args.at(0), args.at(1));
        if new_target == Value::Undefined
            && let (ValueKind::String(source), ValueKind::String(flags)) = (pattern.kind(), flags.kind())
        {
            let compiled = Self::compiled(a, &source, &flags)?;
            return Ok(Self::make(a, &j8framework::RegExpCompiled { source, flags, compiled }).into());
        }
        let pattern_is_regexp = Self::is_regexp(a, pattern)?;
        let mut new_target = new_target;
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

    fn regexp(v: Value<'_>) -> Option<JsObject<'_>> {
        match v.kind() {
            ValueKind::Object(o) if matches!(o.borrow().kind, ObjectKind::RegExp(_)) => Some(o),
            _ => None,
        }
    }

    fn this<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        Self::regexp(this).ok_or_else(|| a.throw(ErrorKind::Type, "RegExp.prototype method called on an incompatible receiver"))
    }

    fn this_object<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<Value<'gc>, Error> {
        match this.kind() {
            ValueKind::Object(_) => Ok(this),
            _ => Err(a.throw(ErrorKind::Type, "RegExp.prototype method called on a non-object")),
        }
    }

    fn data(o: JsObject<'_>) -> (JsString<'_>, JsString<'_>, Gc<'_, Static<Box<dyn core::any::Any + Send + Sync>>>) {
        let d = o.regexp().expect("checked to be a regexp");
        (d.source, d.flags, d.compiled)
    }

    fn with_program<R>(o: JsObject<'_>, f: impl FnOnce(&Compiled) -> R) -> R {
        let compiled = o.regexp().expect("checked to be a regexp").compiled;
        f(compiled.0.downcast_ref::<Compiled>().expect("a compiled regex"))
    }

    fn find(c: &Compiled, units: &[u16], start: usize) -> Option<Found> {
        let m = if c.unicode() { c.regex.find_from_utf16(units, start).next() } else { c.regex.find_from_ucs2(units, start).next() }?;
        let named = m.named_groups().map(|(n, r)| (String::from(n), r)).collect();
        Some(Found { range: m.range(), groups: m.captures, named })
    }

    fn find_ascii(c: &Compiled, s: &str, start: usize) -> Option<Found> {
        if let Some(atom) = &c.atom {
            let rest = &s[start..];
            let at = start + if let [b] = atom.as_bytes() { rest.find(char::from(*b)) } else { rest.find(&**atom) }?;
            return Some(Found { range: at..at + atom.len(), groups: Vec::new(), named: Vec::new() });
        }
        if let Some(d) = &c.dfa {
            let (range, groups) = d.find(s, start)?;
            return Some(Found { range, groups, named: Vec::new() });
        }
        let m = c.regex.find_from(s, start).next()?;
        let named = m.named_groups().map(|(n, r)| (String::from(n), r)).collect();
        Some(Found { range: m.range(), groups: m.captures, named })
    }

    fn span(c: &Compiled, s: &str, start: usize) -> Option<core::ops::Range<usize>> {
        if start > s.len() {
            return None;
        }
        if let Some(atom) = &c.atom {
            if !atom.is_ascii() {
                return None;
            }
            let rest = &s[start..];
            let rel = if let [b] = atom.as_bytes() { rest.find(char::from(*b))? } else { rest.find(atom.as_ref())? };
            let i = start + rel;
            return Some(i..i + atom.len());
        }
        if let Some(d) = &c.dfa {
            return d.find_at(s, start);
        }
        c.regex.find_from(s, start).next().map(|m| m.range())
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

    fn builtin_exec<'gc>(a: &mut Agent<'gc, '_>, o: JsObject<'gc>, s: JsString<'gc>) -> Result<Value<'gc>, Error> {
        let li = a.get_named(o.into(), "lastIndex")?;
        let last = a.to_length(li)?;
        let (global, sticky, indices) = Self::with_program(o, |c| (c.global(), c.sticky(), c.indices()));
        let ascii = s.is_ascii();
        let units: Vec<u16> = if ascii { Vec::new() } else { s.encode_utf16().collect() };
        let text = |a: &Agent<'gc, '_>, r: Option<core::ops::Range<usize>>| if ascii { r.map_or(Value::Undefined, |r| a.piece(s, &s[r]).into()) } else { Self::text(a, &units, r) };
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
        let found = Self::with_program(o, |c| if ascii { Self::find_ascii(c, &s, start) } else { Self::find(c, &units, start) });
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
            if let Some(igo) = ig.as_object() {
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

    fn pristine<'gc>(a: &Agent<'gc, '_>, r: Value<'gc>) -> Option<(JsObject<'gc>, f64)> {
        let o = r.as_object()?;
        if o.regexp().is_none() || o.has_symbols() {
            return None;
        }
        let proto = o.proto()?;
        if !Gc::ptr_eq(proto, a.realm.regexp_proto) {
            return None;
        }
        let p = a.realm.regexp_pristine.get().as_ref()?;
        if o.props().shape() != Some(p.last_index) {
            return None;
        }
        let Slot::Data(v) = o.props().slot(0) else { return None };
        let last = v.as_number()?;
        let props = a.realm.regexp_proto.get().props();
        let intact = props.shape() == Some(p.shape) && p.slots.iter().all(|&(i, orig)| props.word(i).is_some_and(|w| w.bits() == orig.bits()));
        intact.then_some((o, last))
    }

    fn length_from_number(n: f64) -> f64 {
        const I64_SAFE: f64 = 9_223_372_036_854_775_808.0;
        const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
        let n = if n.is_nan() {
            0.0
        } else if n.abs() < I64_SAFE {
            #[expect(clippy::cast_possible_truncation, clippy::cast_precision_loss, reason = "magnitude below 2^63; dropping the fraction is intended")]
            {
                (n as i64) as f64
            }
        } else {
            libm::trunc(n) + 0.0
        };
        n.clamp(0.0, MAX_SAFE_INTEGER)
    }

    fn dollars(s: &str) -> u8 {
        let b = s.as_bytes();
        let mut i = 0;
        let mut kind = 0;
        while i < b.len() {
            if b[i] == b'$' {
                kind = 1;
                if b.get(i + 1) == Some(&b'<') {
                    return 2;
                }
            }
            i += 1;
        }
        kind
    }

    fn set_last<'gc>(a: &Agent<'gc, '_>, o: JsObject<'gc>, i: usize) {
        o.borrow_mut(a.mc).set_at(0, Self::index(i));
    }

    fn find_in(c: &Compiled, s: &str, units: Units<'_>, start: usize) -> Option<Found> {
        match units {
            Units::Ascii(_) => Self::find_ascii(c, s, start),
            Units::Wide(w) => Self::find(c, w, start),
        }
    }

    fn locate(c: &Compiled, s: &str, units: Units<'_>, start: usize) -> Option<core::ops::Range<usize>> {
        match units {
            Units::Ascii(_) => Self::span(c, s, start),
            Units::Wide(w) => {
                let m = if c.unicode() { c.regex.find_from_utf16(w, start).next() } else { c.regex.find_from_ucs2(w, start).next() }?;
                Some(m.range())
            }
        }
    }

    fn quick_exec<'gc>(a: &Agent<'gc, '_>, o: JsObject<'gc>, last: f64, s: &str, units: Units<'_>) -> Result<Option<Found>, Error> {
        let len = units.len();
        let (global, sticky, found) = Self::with_program(o, |c| {
            let (global, sticky) = (c.global(), c.sticky());
            let last = if global || sticky { Self::length_from_number(last) } else { 0.0 };
            #[expect(clippy::cast_precision_loss, reason = "string length is far below 2^53")]
            let fits = last <= len as f64;
            let found = if fits {
                #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "checked not to exceed the length")]
                let start = last as usize;
                Self::find_in(c, s, units, start).filter(|m| !sticky || m.range.start == start)
            } else {
                None
            };
            (global, sticky, found)
        });
        if global || sticky {
            Self::set_last(a, o, found.as_ref().map_or(0, |m| m.range.end));
        }
        Ok(found)
    }

    fn seek(text: &str, needle: &str, from: usize) -> Option<usize> {
        let rest = text.get(from..)?;
        let at = if let [b] = needle.as_bytes() { rest.find(char::from(*b)) } else { rest.find(needle) }?;
        Some(from + at)
    }

    fn splice(text: &str, r: core::ops::Range<usize>, repl: &str) -> SmallStr {
        SmallStr::build(text.len() - r.len() + repl.len(), |out| {
            out.push(&text[..r.start]);
            out.push(repl);
            out.push(&text[r.end..]);
        })
    }

    fn paint(text: &str, repl: &str, find: &impl Fn(usize) -> Option<core::ops::Range<usize>>) -> Option<SmallStr> {
        let n = text.len();
        let step = |r: &core::ops::Range<usize>| if r.is_empty() { r.end + 1 } else { r.end };
        let (mut at, mut prev, mut len, mut any) = (0, 0, 0, false);
        while at <= n
            && let Some(r) = find(at)
        {
            any = true;
            len += r.start - prev + repl.len();
            prev = r.end;
            at = step(&r);
        }
        if !any {
            return None;
        }
        len += n - prev;
        Some(SmallStr::build(len, |out| {
            let (mut at, mut prev) = (0, 0);
            while at <= n
                && let Some(r) = find(at)
            {
                out.push(&text[prev..r.start]);
                out.push(repl);
                prev = r.end;
                at = step(&r);
            }
            out.push(&text[prev..]);
        }))
    }

    fn swap(text: &str, needle: &str, repl: &str) -> Option<SmallStr> {
        let first = Self::seek(text, needle, 0)?;
        if let ([from], [to]) = (needle.as_bytes(), repl.as_bytes())
            && from.is_ascii()
            && to.is_ascii()
        {
            return Some(SmallStr::build(text.len(), |out| out.push_swapped(text, *from, *to)));
        }
        let len = if needle.len() == repl.len() {
            text.len()
        } else {
            let (mut count, mut at) = (0, Some(first));
            while let Some(i) = at {
                count += 1;
                at = Self::seek(text, needle, i + needle.len());
            }
            text.len() - count * needle.len() + count * repl.len()
        };
        Some(SmallStr::build(len, |out| {
            let (mut prev, mut at) = (0, Some(first));
            while let Some(i) = at {
                out.push(&text[prev..i]);
                out.push(repl);
                prev = i + needle.len();
                at = Self::seek(text, needle, prev);
            }
            out.push(&text[prev..]);
        }))
    }

    fn replace_fast<'gc>(a: &Agent<'gc, '_>, o: JsObject<'gc>, last: f64, s: JsString<'gc>, repl: &str) -> Value<'gc> {
        let text: &str = &s;
        let buf = Self::with_program(o, |c| {
            if c.global() {
                let buf = if !c.sticky()
                    && let Some(atom) = c.atom.as_deref()
                {
                    Self::swap(text, atom, repl)
                } else {
                    let sticky = c.sticky();
                    Self::paint(text, repl, &|at| Self::span(c, text, at).filter(|r| !sticky || r.start == at))
                };
                Self::set_last(a, o, 0);
                return buf;
            }
            let cursor = if c.sticky() { Self::length_from_number(last) } else { 0.0 };
            #[expect(clippy::cast_precision_loss, reason = "string length is far below 2^53")]
            let fits = cursor <= text.len() as f64;
            let found = if fits {
                #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "checked not to exceed the length")]
                let start = cursor as usize;
                Self::span(c, text, start).filter(|r| !c.sticky() || r.start == start)
            } else {
                None
            };
            if c.sticky() {
                Self::set_last(a, o, found.as_ref().map_or(0, |r| r.end));
            }
            found.map(|r| Self::splice(text, r, repl))
        });
        match buf {
            Some(buf) => a.string_from(&buf, true).into(),
            None => s.into(),
        }
    }

    fn quick_replace<'gc>(a: &mut Agent<'gc, '_>, o: JsObject<'gc>, last: f64, s: JsString<'gc>, with: JsString<'gc>) -> Result<Option<Value<'gc>>, Error> {
        let dollars = Self::dollars(&with);
        if dollars == 2 && Self::with_program(o, |c| c.named) {
            return Ok(None);
        }
        if dollars == 0 && s.unit_ascii() && with.unit_ascii() {
            return Ok(Some(Self::replace_fast(a, o, last, s, &with)));
        }
        let units = s.units();
        let (global, unicode) = Self::with_program(o, |c| (c.global(), c.unicode()));
        let mut found = Vec::new();
        if global {
            let mut start = 0;
            while start <= units.len() {
                let Some(m) = Self::with_program(o, |c| Self::find_in(c, &s, units, start).filter(|m| !c.sticky() || m.range.start == start)) else { break };
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
        if found.is_empty() {
            return Ok(Some(s.into()));
        }
        let units = units.to_vec();
        let template: Vec<u16> = with.encode_utf16().collect();
        let dollar = dollars != 0;
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

    fn regexp_exec<'gc>(a: &mut Agent<'gc, '_>, r: Value<'gc>, s: JsString<'gc>) -> Result<Value<'gc>, Error> {
        let exec = a.get_named(r, "exec")?;
        if a.is_callable(exec) {
            let result = a.call(exec, r, &[s.into()])?;
            if !matches!(result.kind(), ValueKind::Object(_) | ValueKind::Null) {
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
            return Ok(Self::fast_test(a, o, last, &s).into());
        }
        Ok((Self::regexp_exec(a, r, s)? != Value::Null).into())
    }

    fn fast_test<'gc>(a: &Agent<'gc, '_>, o: JsObject<'gc>, last: f64, s: &j8framework::JsStr) -> bool {
        Self::with_program(o, |c| {
            if !c.global() && !c.sticky() {
                return Self::matches(c, s);
            }
            let units = s.units();
            let len = units.len();
            let cursor = Self::length_from_number(last);
            #[expect(clippy::cast_precision_loss, reason = "string length is far below 2^53")]
            let fits = cursor <= len as f64;
            let found = if fits {
                #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "checked not to exceed the length")]
                let start = cursor as usize;
                Self::locate(c, s, units, start).filter(|r| !c.sticky() || r.start == start)
            } else {
                None
            };
            Self::set_last(a, o, found.as_ref().map_or(0, |r| r.end));
            found.is_some()
        })
    }

    fn matches(c: &Compiled, s: &j8framework::JsStr) -> bool {
        if s.unit_ascii() {
            return match (&c.atom, &c.dfa) {
                (Some(atom), _) => {
                    if let [b] = atom.as_bytes() { s.contains(char::from(*b)) } else { s.contains(atom.as_ref()) }
                }
                (None, Some(d)) => d.is_match(s),
                (None, None) => c.regex.find_from(s, 0).next().is_some(),
            };
        }
        Self::locate(c, s, s.units(), 0).is_some()
    }

    fn to_string<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let r = Self::this_object(a, this)?;
        let p = a.get_named(r, "source")?;
        let p = a.to_string(p)?;
        let f = a.get_named(r, "flags")?;
        let f = a.to_string(f)?;
        Ok(a.string(&alloc::format!("/{}/{}", &**p, &**f)).into())
    }

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
        if let Some(o) = this.as_object()
            && Gc::ptr_eq(o, a.realm.regexp_proto)
        {
            return Ok(a.string("(?:)").into());
        }
        let o = Self::this(a, this)?;
        let s = Self::escape_pattern(&Self::data(o).0);
        Ok(a.string(&s).into())
    }

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

    fn flag<'gc, const C: char>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let Some(o) = this.as_object() else { return Err(a.throw(ErrorKind::Type, "RegExp flag getter called on a non-object")) };
        match Self::regexp(this) {
            Some(r) => Ok(Self::with_program(r, |c| c.has(C)).into()),
            None if Gc::ptr_eq(o, a.realm.regexp_proto) => Ok(Value::Undefined),
            None => Err(a.throw(ErrorKind::Type, "RegExp flag getter called on an incompatible receiver")),
        }
    }

    fn get_flags<'gc>(a: &mut Agent<'gc, '_>, r: Value<'gc>) -> Result<String, Error> {
        let f = a.get_named(r, "flags")?;
        Ok(String::from(&**a.to_string(f)?))
    }

    fn builtin_bits<'gc>(a: &Agent<'gc, '_>, r: Value<'gc>) -> Option<u16> {
        let o = Self::regexp(r)?;
        let saved = a.realm.regexp_pristine.get().as_ref()?;
        if o.props().shape() != Some(saved.last_index) || o.proto().is_none_or(|p| !Gc::ptr_eq(p, a.realm.regexp_proto)) {
            return None;
        }
        let props = a.realm.regexp_proto.get().props();
        let intact = props.shape() == Some(saved.shape) && saved.slots.iter().all(|&(i, orig)| props.word(i).is_some_and(|w| w.bits() == orig.bits()));
        intact.then(|| Self::with_program(o, |c| c.flags))
    }

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

    fn step_last_index<'gc>(a: &mut Agent<'gc, '_>, r: Value<'gc>, units: &[u16], unicode: bool) -> Result<(), Error> {
        let li = a.get_named(r, "lastIndex")?;
        let this_index = a.to_length(li)?;
        a.set_named(r, "lastIndex", Self::advance(units, this_index, unicode).into())
    }

    fn match_symbol<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let r = Self::this_object(a, this)?;
        let s = a.to_string(args.at(0))?;
        let (global, unicode) = if let Some(bits) = Self::builtin_bits(a, r) {
            (Compiled::on(bits, Compiled::GLOBAL), Compiled::on(bits, Compiled::UNICODE | Compiled::UNICODE_SETS))
        } else {
            let flags = Self::get_flags(a, r)?;
            (flags.contains('g'), flags.contains('u') || flags.contains('v'))
        };
        if !global {
            return Self::regexp_exec(a, r, s);
        }
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
        let default = a.realm.global.borrow().own(a.mc, "RegExp").unwrap_or_default();
        let ctor = Buffers::species_constructor(a, r, default)?;
        let flags = Self::get_flags(a, r)?;
        let f = a.string(&flags);
        let matcher = a.construct(ctor, &[r, f.into()], ctor)?;
        let li = a.get_named(r, "lastIndex")?;
        let last = a.to_length(li)?;
        a.set_named(matcher, "lastIndex", last.into())?;
        let Some(matcher) = matcher.as_object() else { return Err(a.throw(ErrorKind::Type, "species constructor returned a non-object")) };
        let kind = ObjectKind::RegExpStringIterator { matcher, string: s, global: flags.contains('g'), unicode: flags.contains('u') || flags.contains('v'), done: false };
        Ok(ObjectData { kind, proto: Some(a.realm.regexp_string_iterator_proto), ..ObjectData::default() }.alloc(a.mc).into())
    }

    fn string_iterator_next<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let state = match this.kind() {
            ValueKind::Object(o) => match o.borrow().kind {
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
        let default = a.realm.global.borrow().own(a.mc, "RegExp").unwrap_or_default();
        let ctor = Buffers::species_constructor(a, r, default)?;
        let (unicode, new_flags) = if let Some(bits) = Self::builtin_bits(a, r) {
            let flags = Compiled::text(bits);
            (Compiled::on(bits, Compiled::UNICODE | Compiled::UNICODE_SETS), if Compiled::on(bits, Compiled::STICKY) { flags } else { alloc::format!("{flags}y") })
        } else {
            let flags = Self::get_flags(a, r)?;
            (flags.contains('u') || flags.contains('v'), if flags.contains('y') { flags } else { alloc::format!("{flags}y") })
        };
        let nf = a.string(&new_flags);
        let splitter = a.construct(ctor, &[r, nf.into()], ctor)?;
        let arr = a.array(Vec::new());
        let lim = match args.at(1).kind() {
            ValueKind::Undefined => MAX_SPLIT,
            v => f64::from(a.to_uint32(Value::from(v))?),
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

    fn escape<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let ValueKind::String(s) = args.at(0).kind() else { return Err(a.throw(ErrorKind::Type, "RegExp.escape requires a string")) };
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

    fn subject<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>) -> Result<Value<'gc>, Error> {
        if this.is_nullish() {
            return Err(a.throw(ErrorKind::Type, "String.prototype method called on null or undefined"));
        }
        Ok(this)
    }

    fn method_of<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>, sym: j8framework::JsSymbol<'gc>) -> Result<Option<Value<'gc>>, Error> {
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

    fn dispatch<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, arg: Value<'gc>, sym: j8framework::JsSymbol<'gc>, flags: Value<'gc>) -> Result<Value<'gc>, Error> {
        let o = Self::subject(a, this)?;
        if arg.as_object().is_some()
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
        if arg.as_object().is_some() {
            Self::require_global(a, arg, "matchAll")?;
        }
        let g = a.string("g");
        Self::dispatch(a, this, arg, a.realm.symbols.match_all, g.into())
    }

    fn replace_impl<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>], all: bool) -> Result<Value<'gc>, Error> {
        let o = Self::subject(a, this)?;
        let (search, with) = (args.at(0), args.at(1));
        if search.as_object().is_some() {
            if all {
                Self::require_global(a, search, "replaceAll")?;
            }
            if let Some(f) = Self::method_of(a, search, a.realm.symbols.replace)? {
                if a.realm.regexp_pristine.get().as_ref().is_some_and(|p| f.as_object().is_some_and(|f| Gc::ptr_eq(f, p.replace))) {
                    return Self::replace_symbol(a, search, &[o, with]);
                }
                return a.call(f, search, &[o, with]);
            }
        }
        let s = a.to_string(o)?;
        let needle = a.to_string(search)?;
        if !a.is_callable(with) {
            let repl = a.to_string(with)?;
            if s.unit_ascii() && needle.unit_ascii() && repl.unit_ascii() && Self::dollars(&repl) == 0 {
                return Ok(Self::replace_ascii(a, s, &needle, &repl, all));
            }
            return Self::replace_pattern(a, s, needle, repl.into(), false, all);
        }
        Self::replace_pattern(a, s, needle, with, true, all)
    }

    fn replace_gaps<'gc>(a: &Agent<'gc, '_>, text: &str, repl: &str, all: bool) -> Value<'gc> {
        if !all {
            return a.string_from(&Self::splice(text, 0..0, repl), true).into();
        }
        let buf = SmallStr::build(repl.len() * (text.len() + 1) + text.len(), |out| {
            for i in 0..text.len() {
                out.push(repl);
                out.push(&text[i..=i]);
            }
            out.push(repl);
        });
        a.string_from(&buf, true).into()
    }

    fn replace_ascii<'gc>(a: &Agent<'gc, '_>, s: JsString<'gc>, needle: &str, repl: &str, all: bool) -> Value<'gc> {
        let text: &str = &s;
        if needle.is_empty() {
            if repl.is_empty() {
                return s.into();
            }
            return Self::replace_gaps(a, text, repl, all);
        }
        let buf = if all { Self::swap(text, needle, repl) } else { Self::seek(text, needle, 0).map(|p| Self::splice(text, p..p + needle.len(), repl)) };
        match buf {
            Some(buf) => a.string_from(&buf, true).into(),
            None => s.into(),
        }
    }

    fn replace_pattern<'gc>(a: &mut Agent<'gc, '_>, s: JsString<'gc>, needle: JsString<'gc>, with: Value<'gc>, functional: bool, all: bool) -> Result<Value<'gc>, Error> {
        let units: Vec<u16> = s.encode_utf16().collect();
        let needle_units: Vec<u16> = needle.encode_utf16().collect();
        let template: Vec<u16> = if functional { Vec::new() } else { a.to_string(with)?.encode_utf16().collect() };
        let mut positions = Vec::new();
        let mut at = Strings::find(&units, &needle_units, 0);
        while let Some(p) = at {
            positions.push(p);
            if !all {
                break;
            }
            at = Strings::find(&units, &needle_units, p + needle_units.len().max(1));
        }
        let mut out = Vec::new();
        let mut end = 0;
        for p in positions {
            out.extend_from_slice(&units[end..p]);
            let replacement: Vec<u16> = if functional {
                let v = a.call(with, Value::Undefined, &[needle.into(), Self::index(p), s.into()])?;
                a.to_string(v)?.encode_utf16().collect()
            } else {
                Self::substitution(a, &needle_units, &units, p, &[], Value::Undefined, &template)?
            };
            out.extend(replacement);
            end = p + needle_units.len();
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
        if sep.as_object().is_some()
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
        r.regexp_ctor.borrow_mut(self.mc).kind = ObjectKind::Native(j8framework::Native::function(Regexps::construct));
        self.method(proto, "exec", Regexps::exec, 1);
        self.method(proto, "test", Regexps::test, 1);
        self.method(proto, "toString", Regexps::to_string, 0);
        self.method(proto, "compile", Regexps::compile_method, 2);
        self.getter(proto, "source", Regexps::source);
        self.getter(proto, "flags", Regexps::flags);
        let flags: [(&str, j8framework::NativeFn); 8] = [
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
        let symbols: [(j8framework::JsSymbol<'gc>, j8framework::NativeFn, u32); 5] = [
            (r.symbols.match_, Regexps::match_symbol, 1),
            (r.symbols.match_all, Regexps::match_all_symbol, 1),
            (r.symbols.replace, Regexps::replace_symbol, 2),
            (r.symbols.search, Regexps::search_symbol, 1),
            (r.symbols.split, Regexps::split_symbol, 2),
        ];
        let methods = symbols.map(|(sym, f, n)| r.symbol_method(self.mc, proto, sym, f, n));
        let last_index = {
            let sample = ObjectData { proto: Some(proto), ..ObjectData::default() }.alloc(self.mc);
            sample.borrow_mut(self.mc).define("lastIndex", Property { slot: Slot::Data(0.0.into()), enumerable: false, writable: true, configurable: false });
            sample.props().shape().expect("a lastIndex shape")
        };
        let pristine = {
            let d = proto.borrow();
            let slots = ["exec", "flags"].into_iter().chain(FLAG_PROPERTIES.map(|(_, n)| n)).filter_map(|n| d.props().position(n).and_then(|i| Some((i, d.props().word(i)?)))).collect();
            d.props().shape().map(|shape| j8framework::RegExpPristine { shape, last_index, slots, replace: methods[2] })
        };
        r.regexp_pristine.set(self.mc, pristine);
        r.regexp_literal.set(self.mc, j8framework::Static(Some(Regexps::literal)));
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
