use j8framework::ValueKind;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use j8framework::{Agent, Error, ErrorKind, JsObject, JsString, ObjectData, Property, Shape, Slot, SmallStr, Value};

use crate::{Args, Installer};

/// Maximum number of indentation characters for `JSON.stringify`
const MAX_GAP: usize = 10;

/// Parser for `JSON.parse`, following the spec's JSON grammar (only the four whitespace characters, no trailing commas,
/// no leading zeros in numbers).
struct Parser<'s, 'a, 'gc, 'x> {
    a: &'a mut Agent<'gc, 'x>,
    text: &'s str,
    pos: usize,
    /// Key sequences seen and the shapes they built: an object with the same keys takes its shape at once
    shapes: Vec<(Vec<String>, alloc::sync::Arc<Shape>)>,
}

/// How many key sequences a parse remembers shapes for
const SHAPES_CACHED: usize = 8;

impl<'s, 'gc> Parser<'s, '_, 'gc, '_> {
    fn error(&self, what: &str) -> Error {
        let found = self.text[self.pos..].chars().next().map_or_else(|| String::from("end of JSON input"), |c| alloc::format!("token '{c}'"));
        self.a.throw(ErrorKind::Syntax, &alloc::format!("Unexpected {found} in JSON at position {} ({what})", self.pos))
    }

    fn skip_ws(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.text.as_bytes().get(self.pos) {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn eat(&mut self, lit: &str) -> bool {
        if self.text[self.pos..].starts_with(lit) {
            self.pos += lit.len();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, b: u8, what: &str) -> Result<(), Error> {
        if self.peek() == Some(b) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.error(what))
        }
    }

    fn value(&mut self) -> Result<Value<'gc>, Error> {
        self.skip_ws();
        let v = match self.peek() {
            Some(b'{') => self.object()?,
            Some(b'[') => self.array()?,
            Some(b'"') => {
                let s = self.string()?;
                self.a.string(&s).into()
            }
            Some(b'-' | b'0'..=b'9') => self.number()?,
            _ if self.eat("true") => true.into(),
            _ if self.eat("false") => false.into(),
            _ if self.eat("null") => Value::Null,
            _ => return Err(self.error("expected a value")),
        };
        self.skip_ws();
        Ok(v)
    }

    fn object(&mut self) -> Result<Value<'gc>, Error> {
        self.pos += 1;
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(self.a.object().into());
        }
        let mut pairs = Vec::new();
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.error("expected a property name"));
            }
            let k = self.string()?;
            self.skip_ws();
            self.expect(b':', "expected ':'")?;
            let v = self.value()?;
            pairs.push((k, v));
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(self.build(pairs).into());
                }
                _ => return Err(self.error("expected ',' or '}'")),
            }
        }
    }

    /// A fresh ordinary object with these properties in order. CreateDataProperty always succeeds on it; `__proto__`
    /// is just an ordinary key, and a duplicate key replaces the previous one entirely. Keys seen before in the same
    /// order (all distinct, none an index) give the shape they built then, filled in one step.
    fn build(&mut self, pairs: Vec<(alloc::borrow::Cow<'s, str>, Value<'gc>)>) -> JsObject<'gc> {
        let a = &*self.a;
        let o = a.object_with(pairs.len());
        if let Some((_, shape)) = self.shapes.iter().find(|(keys, _)| keys.len() == pairs.len() && keys.iter().zip(&pairs).all(|(k, (p, _))| **k == **p)) {
            o.fill(a.mc, shape, pairs.iter().map(|&(_, v)| Slot::Data(v)));
            return o;
        }
        for (k, v) in &pairs {
            o.borrow_mut(a.mc).define(k, Property::data(*v));
        }
        if self.shapes.len() < SHAPES_CACHED
            && let Some(shape) = o.get().props().shape().filter(|s| s.len() == pairs.len())
        {
            self.shapes.push((pairs.into_iter().map(|(k, _)| String::from(k)).collect(), shape));
        }
        o
    }

    fn array(&mut self) -> Result<Value<'gc>, Error> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(self.a.array(items).into());
        }
        loop {
            items.push(self.value()?);
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(self.a.array(items).into());
                }
                _ => return Err(self.error("expected ',' or ']'")),
            }
        }
    }

    /// Four hex digits.
    fn hex4(&mut self) -> Result<u16, Error> {
        let digits = self.text.get(self.pos..self.pos + 4).filter(|d| d.bytes().all(|c| c.is_ascii_hexdigit())).ok_or_else(|| self.error("bad \\u escape"))?;
        self.pos += 4;
        u16::from_str_radix(digits, 16).map_err(|_| self.error("bad \\u escape"))
    }

    /// A string (currently at the opening quote). `\u` surrogate pairs are combined; lone surrogates become U+FFFD.
    fn string(&mut self) -> Result<alloc::borrow::Cow<'s, str>, Error> {
        // No escapes (the common case): borrow the source directly; `"`, `\\` and control characters never occur inside
        // UTF-8 multi-byte sequences, so scanning by byte is fine
        let start = self.pos + 1;
        let text: &'s str = self.text;
        let end = text.as_bytes()[start..].iter().position(|&b| b == b'"' || b == b'\\' || b < 0x20).map(|i| start + i);
        if let Some(end) = end
            && text.as_bytes()[end] == b'"'
        {
            self.pos = end + 1;
            return Ok(alloc::borrow::Cow::Borrowed(&text[start..end]));
        }
        self.escaped().map(alloc::borrow::Cow::Owned)
    }

    /// A string with escapes (currently at the opening quote).
    fn escaped(&mut self) -> Result<String, Error> {
        self.pos += 1;
        let mut out = String::new();
        let mut units: Vec<u16> = Vec::new();
        loop {
            let c = self.text[self.pos..].chars().next().ok_or_else(|| self.error("unterminated string"))?;
            if c != '\\' && !units.is_empty() {
                out.push_str(&String::from_utf16_lossy(&units));
                units.clear();
            }
            match c {
                '"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                '\u{0}'..='\u{1f}' => return Err(self.error("bad control character in string")),
                '\\' => {
                    self.pos += 1;
                    let e = self.peek().ok_or_else(|| self.error("unterminated string"))?;
                    self.pos += 1;
                    let simple = match e {
                        b'"' => Some('"'),
                        b'\\' => Some('\\'),
                        b'/' => Some('/'),
                        b'b' => Some('\u{8}'),
                        b'f' => Some('\u{c}'),
                        b'n' => Some('\n'),
                        b'r' => Some('\r'),
                        b't' => Some('\t'),
                        b'u' => None,
                        _ => {
                            self.pos -= 1;
                            return Err(self.error("bad escape"));
                        }
                    };
                    match simple {
                        Some(ch) => {
                            if !units.is_empty() {
                                out.push_str(&String::from_utf16_lossy(&units));
                                units.clear();
                            }
                            out.push(ch);
                        }
                        None => units.push(self.hex4()?),
                    }
                }
                c => {
                    out.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
    }

    /// A number: `-? (0 | [1-9][0-9]*) (. [0-9]+)? ([eE] [+-]? [0-9]+)?`.
    fn number(&mut self) -> Result<Value<'gc>, Error> {
        let start = self.pos;
        let digits = |p: &mut Self| {
            let s = p.pos;
            while let Some(b'0'..=b'9') = p.peek() {
                p.pos += 1;
            }
            p.pos > s
        };
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                digits(self);
            }
            _ => return Err(self.error("bad number")),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !digits(self) {
                return Err(self.error("bad number"));
            }
        }
        if let Some(b'e' | b'E') = self.peek() {
            self.pos += 1;
            if let Some(b'+' | b'-') = self.peek() {
                self.pos += 1;
            }
            if !digits(self) {
                return Err(self.error("bad number"));
            }
        }
        let x: f64 = self.text[start..self.pos].parse().map_err(|_| self.error("bad number"))?;
        Ok(x.into())
    }
}

/// State of `JSON.stringify`: indentation, key allowlist, replacer function, objects being serialized (cycle
/// detection).
struct Writer<'gc> {
    gap: String,
    indent: String,
    allow: Option<Vec<JsString<'gc>>>,
    replacer: Option<Value<'gc>>,
    stack: Vec<JsObject<'gc>>,
}

impl<'gc> Writer<'gc> {
    /// A quoted and escaped string.
    fn quote(out: &mut String, s: &str) {
        out.reserve(s.len() + 2);
        out.push('"');
        // Copy runs that need no escaping verbatim (only ASCII needs escaping, and it never occurs inside UTF-8
        // multi-byte sequences)
        let mut from = 0;
        for (i, &b) in s.as_bytes().iter().enumerate() {
            let esc = match b {
                b'"' => "\\\"",
                b'\\' => "\\\\",
                0x8 => "\\b",
                0xc => "\\f",
                b'\n' => "\\n",
                b'\r' => "\\r",
                b'\t' => "\\t",
                0..0x20 => "",
                _ => continue,
            };
            out.push_str(&s[from..i]);
            if esc.is_empty() {
                // Writing into a String can't fail
                let _ = write!(out, "\\u{b:04x}");
            } else {
                out.push_str(esc);
            }
            from = i + 1;
        }
        out.push_str(&s[from..]);
        out.push('"');
    }

    /// Own enumerable string keys (array indices first). Keys aren't created as JS strings (short ones are inline);
    /// they're created only when passed to `toJSON` / the replacer.
    /// The enumerable own keys, each with its entry index when `o` is a plain fast-mode object (read in place while
    /// its shape stays the same, see [`Writer::object`])
    fn keys(o: JsObject<'gc>) -> Vec<(SmallStr, Option<usize>)> {
        let d = o.borrow();
        let mut keys = Vec::new();
        if o.is_array() {
            keys.extend((0..o.elements().len()).map(|i| (j8framework::number_text(f64::from(u32::try_from(i).unwrap_or(u32::MAX))), None)));
        }
        let props = d.props();
        let direct = o.get().is_plain() && !props.is_dict();
        keys.extend((0..props.len()).filter(|&i| props.attrs(i).enumerable()).map(|i| (props.key(i), direct.then_some(i))));
        keys
    }

    /// SerializeJSONProperty: `toJSON`, the replacer, output by type; `undefined` / functions / Symbols give `None`
    /// (omitted).
    /// `v.toJSON`: `undefined` straight away when no object on an object's prototype chain has the key (all plain
    /// objects: then the lookup is unobservable), otherwise a Get
    fn to_json(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Value<'gc>, Error> {
        const KEY: &str = "toJSON";
        let mut at = v.as_object();
        while let Some(o) = at {
            let d = o.get();
            if !d.is_plain() || d.props().position(KEY).is_some() {
                return a.get_named(v, KEY);
            }
            at = d.proto();
        }
        if v.as_object().is_some() { Ok(Value::Undefined) } else { a.get_named(v, KEY) }
    }

    /// `read`: the value when the caller read it in place (an unobservable Get of a data property), else it is got
    /// here
    fn property(&mut self, a: &mut Agent<'gc, '_>, holder: Value<'gc>, key: &SmallStr, read: Option<Value<'gc>>, out: &mut String) -> Result<bool, Error> {
        let mut v = match read {
            Some(v) => v,
            None => a.get_named(holder, key)?,
        };
        if matches!(v.kind(), ValueKind::Object(_) | ValueKind::BigInt(_)) {
            let to_json = Self::to_json(a, v)?;
            if a.is_callable(to_json) {
                let k = a.string(key);
                v = a.call(to_json, v, &[k.into()])?;
            }
        }
        if let Some(r) = self.replacer {
            let k = a.string(key);
            v = a.call(r, holder, &[k.into(), v])?;
        }
        match v.kind() {
            ValueKind::Null | ValueKind::Number(_) if !matches!(v.kind(), ValueKind::Number(x) if x.is_finite()) => out.push_str("null"),
            ValueKind::Bool(true) => out.push_str("true"),
            ValueKind::Bool(false) => out.push_str("false"),
            ValueKind::String(s) => Self::quote(out, &s),
            ValueKind::Number(x) => out.push_str(&j8framework::number_text(x)),
            ValueKind::BigInt(_) => return Err(a.throw(ErrorKind::Type, "Do not know how to serialize a BigInt")),
            ValueKind::Object(o) if !o.borrow().kind.is_callable() => {
                if self.stack.iter().any(|&s| j8framework::Gc::ptr_eq(s, o)) {
                    return Err(a.throw(ErrorKind::Type, "Converting circular structure to JSON"));
                }
                self.stack.push(o);
                let array = o.is_array();
                if array { self.array(a, o, out)? } else { self.object(a, o, out)? }
                self.stack.pop();
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Newline and indentation at the start of a level / before each item (nothing if there is no gap).
    fn newline(&self, out: &mut String) {
        if !self.gap.is_empty() {
            out.push('\n');
            out.push_str(&self.indent);
        }
    }

    fn object(&mut self, a: &mut Agent<'gc, '_>, o: JsObject<'gc>, out: &mut String) -> Result<(), Error> {
        let outer = self.indent.clone();
        self.indent.push_str(&self.gap);
        let keys = match &self.allow {
            Some(k) => k.iter().map(|s| (s.text().clone(), None)).collect(),
            None => Self::keys(o),
        };
        // While the shape is the one the keys came from, entry `i` is still that key's data property (serializing a
        // value may run `toJSON` / the replacer, which can change the object: then the rest are got by name)
        let shape = o.get().props().shape_ptr();
        out.push('{');
        let mut first = true;
        for (k, at) in keys {
            let read = at.filter(|_| shape.is_some() && o.get().props().shape_ptr() == shape).and_then(|i| o.get().props().read_data(i));
            let mark = out.len();
            if !first {
                out.push(',');
            }
            self.newline(out);
            Self::quote(out, &k);
            out.push(':');
            if !self.gap.is_empty() {
                out.push(' ');
            }
            if self.property(a, o.into(), &k, read, out)? {
                first = false;
            } else {
                out.truncate(mark);
            }
        }
        self.indent = outer;
        if !first {
            self.newline(out);
        }
        out.push('}');
        Ok(())
    }

    /// Array with no replacer, no indentation, dense, all elements primitive: primitives don't look up `toJSON` and
    /// reading each element is unobservable, so chunks are written as JSON in parallel on the thread pool, then
    /// concatenated. Returns `false` otherwise (nothing written).
    fn plain_array(&self, a: &Agent<'gc, '_>, o: JsObject<'gc>, out: &mut String) -> bool {
        /// Shape of one element (no GC pointers, so it can be handed to other threads)
        #[derive(Clone, Copy)]
        enum Part<'s> {
            Number(f64),
            Text(&'s str),
            Raw(&'static str),
        }
        if self.replacer.is_some() || !self.gap.is_empty() {
            return false;
        }
        if !o.is_array() {
            return false;
        }
        let d = o.borrow();
        let items = o.elements().to_vec();
        if d.sparse_length() as usize > items.len() || !items.iter().all(|v| matches!(v.kind(), ValueKind::Number(_) | ValueKind::String(_) | ValueKind::Bool(_) | ValueKind::Null | ValueKind::Undefined)) {
            return false;
        }
        let parts: Vec<Part<'_>> = items
            .iter()
            .map(|v| match v.kind() {
                ValueKind::Number(x) if x.is_finite() => Part::Number(x),
                ValueKind::String(s) => Part::Text(j8framework::Gc::get(s)),
                ValueKind::Bool(true) => Part::Raw("true"),
                ValueKind::Bool(false) => Part::Raw("false"),
                _ => Part::Raw("null"),
            })
            .collect();
        let pieces = a.par().chunks(&parts, &|chunk: &[Part<'_>]| {
            let mut s = String::new();
            for (i, p) in chunk.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                match *p {
                    Part::Number(x) => s.push_str(&j8framework::number_text(x)),
                    Part::Text(t) => Self::quote(&mut s, t),
                    Part::Raw(r) => s.push_str(r),
                }
            }
            s
        });
        out.push('[');
        for (i, p) in pieces.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(p);
        }
        out.push(']');
        true
    }

    fn array(&mut self, a: &mut Agent<'gc, '_>, o: JsObject<'gc>, out: &mut String) -> Result<(), Error> {
        if self.plain_array(a, o, out) {
            return Ok(());
        }
        let outer = self.indent.clone();
        self.indent.push_str(&self.gap);
        let len = a.get_named(o.into(), "length")?;
        let len = a.to_uint32(len)?;
        out.push('[');
        for i in 0..len {
            if i > 0 {
                out.push(',');
            }
            self.newline(out);
            let k = j8framework::number_text(f64::from(i));
            if !self.property(a, o.into(), &k, None, out)? {
                out.push_str("null");
            }
        }
        self.indent = outer;
        if len > 0 {
            self.newline(out);
        }
        out.push(']');
        Ok(())
    }
}

/// `JSON`.
struct Json;

impl Json {
    /// `JSON.parse(text, reviver)`.
    fn parse<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let text = a.to_string(args.at(0))?;
        let mut p = Parser { a, text: &text, pos: 0, shapes: Vec::new() };
        let v = p.value()?;
        if p.pos != text.len() {
            return Err(p.error("unexpected trailing input"));
        }
        let reviver = args.at(1);
        if !a.is_callable(reviver) {
            return Ok(v);
        }
        let root = a.object();
        root.borrow_mut(a.mc).set("", v);
        let key = a.string("");
        Self::revive(a, root.into(), key, reviver)
    }

    /// InternalizeJSONProperty: process children bottom-up first, then call `reviver.call(holder, key, value)`;
    /// `undefined` deletes the property.
    fn revive<'gc>(a: &mut Agent<'gc, '_>, holder: Value<'gc>, key: JsString<'gc>, reviver: Value<'gc>) -> Result<Value<'gc>, Error> {
        let v = a.get_named(holder, &key)?;
        if let Some(o) = v.as_object() {
            let keys = Writer::keys(o);
            for (k, _) in keys {
                let k = a.string(&k);
                let nv = Self::revive(a, v, k, reviver)?;
                if nv == Value::Undefined {
                    a.delete(v, &k)?;
                } else {
                    a.create_data_property(o, j8framework::Key::Str(&k), nv)?;
                }
            }
        }
        a.call(reviver, holder, &[key.into(), v])
    }

    /// `JSON.stringify(value, replacer, space)`.
    fn stringify<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (replacer, allow) = match args.at(1).kind() {
            f if a.is_callable(Value::from(f)) => (Some(Value::from(f)), None),
            ValueKind::Object(o) if o.is_array() => {
                let items = a.to_list(Value::Object(o))?;
                let mut keys: Vec<JsString<'gc>> = Vec::new();
                for item in items {
                    let k = match item.kind() {
                        ValueKind::String(s) => Some(s),
                        ValueKind::Number(_) => Some(a.to_string(item)?),
                        _ => None,
                    };
                    if let Some(k) = k
                        && !keys.iter().any(|e| ***e == **k)
                    {
                        keys.push(k);
                    }
                }
                (None, Some(keys))
            }
            _ => (None, None),
        };
        let gap = match args.at(2).kind() {
            ValueKind::Number(n) => {
                #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to [0, MAX_GAP]")]
                let n = libm::trunc(n.clamp(0.0, 10.0)) as usize;
                " ".repeat(n.min(MAX_GAP))
            }
            ValueKind::String(s) => s.chars().take(MAX_GAP).collect(),
            _ => String::new(),
        };
        let mut w = Writer { gap, indent: String::new(), allow, replacer, stack: Vec::new() };
        let wrapper = ObjectData { proto: Some(a.realm.object_proto), ..ObjectData::default() }.alloc(a.mc);
        wrapper.borrow_mut(a.mc).set("", args.at(0));
        let key = SmallStr::from("");
        let mut out = String::new();
        if w.property(a, wrapper.into(), &key, None, &mut out)? { Ok(a.string(&out).into()) } else { Ok(Value::Undefined) }
    }
}

impl Installer<'_, '_> {
    pub(crate) fn json(&mut self) {
        let j = self.plain();
        self.method(j, "parse", Json::parse, 2);
        self.method(j, "stringify", Json::stringify, 3);
        self.set(self.realm.global, "JSON", j.into());
    }
}
