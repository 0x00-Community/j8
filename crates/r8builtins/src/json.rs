use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use r8framework::{Agent, Error, ErrorKind, JsObject, JsString, ObjectData, ObjectKind, Property, SmallStr, Value};

use crate::{Args, Installer};

/// `JSON.stringify` 缩进最多这么多个字符
const MAX_GAP: usize = 10;

/// `JSON.parse` 的解析器：按规范的 JSON 文法（只认四种空白、不收尾逗号、数不带前导零）。
struct Parser<'s, 'a, 'gc, 'x> {
    a: &'a mut Agent<'gc, 'x>,
    text: &'s str,
    pos: usize,
}

impl<'gc> Parser<'_, '_, 'gc, '_> {
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
        let o = self.a.object();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(o.into());
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.error("expected a property name"));
            }
            let k = self.string()?;
            self.skip_ws();
            self.expect(b':', "expected ':'")?;
            let v = self.value()?;
            // CreateDataProperty：新建的普通对象一定成；`__proto__` 也只是普通键，重复的键整个换掉
            o.borrow_mut(self.a.mc).define(&k, Property::data(v));
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(o.into());
                }
                _ => return Err(self.error("expected ',' or '}'")),
            }
        }
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

    /// 四位十六进制。
    fn hex4(&mut self) -> Result<u16, Error> {
        let digits = self.text.get(self.pos..self.pos + 4).filter(|d| d.bytes().all(|c| c.is_ascii_hexdigit())).ok_or_else(|| self.error("bad \\u escape"))?;
        self.pos += 4;
        u16::from_str_radix(digits, 16).map_err(|_| self.error("bad \\u escape"))
    }

    /// 字符串（当前在开头的引号上）。`\u` 代理对拼起来，落单的代理换成 U+FFFD。
    fn string(&mut self) -> Result<String, Error> {
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

    /// 数：`-? (0 | [1-9][0-9]*) (. [0-9]+)? ([eE] [+-]? [0-9]+)?`。
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

/// `JSON.stringify` 的状态：缩进、键白名单、替换函数、正在序列化的对象（查环）。
struct Writer<'gc> {
    gap: String,
    indent: String,
    allow: Option<Vec<JsString<'gc>>>,
    replacer: Option<Value<'gc>>,
    stack: Vec<JsObject<'gc>>,
}

impl<'gc> Writer<'gc> {
    /// 带引号与转义的字符串。
    fn quote(out: &mut String, s: &str) {
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if u32::from(c) < 0x20 => {
                    // 写进 String 不会失败
                    let _ = write!(out, "\\u{:04x}", u32::from(c));
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }

    /// 自有可枚举字符串键（数组下标在前）。键不建成 JS 字符串（短的内联），要传给 `toJSON` / 替换函数时才建。
    fn keys(o: JsObject<'gc>) -> Vec<SmallStr> {
        let d = o.borrow();
        let mut keys = Vec::new();
        if let ObjectKind::Array(items) = &d.kind {
            keys.extend((0..items.len()).map(|i| r8framework::number_text(f64::from(u32::try_from(i).unwrap_or(u32::MAX)))));
        }
        keys.extend(d.props.iter().filter(|(_, p)| p.enumerable).map(|(k, _)| k.clone()));
        keys
    }

    /// SerializeJSONProperty：`toJSON`、替换函数、按类型输出；`undefined` / 函数 / Symbol 给 `None`（省掉）。
    fn property(&mut self, a: &mut Agent<'gc, '_>, holder: Value<'gc>, key: &SmallStr, out: &mut String) -> Result<bool, Error> {
        let mut v = a.get_named(holder, key)?;
        if matches!(v, Value::Object(_) | Value::BigInt(_)) {
            let to_json = a.get_named(v, "toJSON")?;
            if a.is_callable(to_json) {
                let k = a.string(key);
                v = a.call(to_json, v, &[k.into()])?;
            }
        }
        if let Some(r) = self.replacer {
            let k = a.string(key);
            v = a.call(r, holder, &[k.into(), v])?;
        }
        match v {
            Value::Null | Value::Number(_) if !matches!(v, Value::Number(x) if x.is_finite()) => out.push_str("null"),
            Value::Bool(true) => out.push_str("true"),
            Value::Bool(false) => out.push_str("false"),
            Value::String(s) => Self::quote(out, &s),
            Value::Number(_) => out.push_str(&a.to_string(v)?),
            Value::BigInt(_) => return Err(a.throw(ErrorKind::Type, "Do not know how to serialize a BigInt")),
            Value::Object(o) if !o.borrow().kind.is_callable() => {
                if self.stack.iter().any(|&s| r8gc::Gc::ptr_eq(s, o)) {
                    return Err(a.throw(ErrorKind::Type, "Converting circular structure to JSON"));
                }
                self.stack.push(o);
                let array = matches!(o.borrow().kind, ObjectKind::Array(_));
                if array { self.array(a, o, out)? } else { self.object(a, o, out)? }
                self.stack.pop();
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// 一层开头 / 每一项之前的换行与缩进（没有 gap 就什么都不加）。
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
            Some(k) => k.iter().map(|s| s.text().clone()).collect(),
            None => Self::keys(o),
        };
        out.push('{');
        let mut first = true;
        for k in keys {
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
            if self.property(a, o.into(), &k, out)? {
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

    fn array(&mut self, a: &mut Agent<'gc, '_>, o: JsObject<'gc>, out: &mut String) -> Result<(), Error> {
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
            let k = r8framework::number_text(f64::from(i));
            if !self.property(a, o.into(), &k, out)? {
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

/// `JSON`。
struct Json;

impl Json {
    /// `JSON.parse(text, reviver)`。
    fn parse<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let text = a.to_string(args.at(0))?;
        let mut p = Parser { a, text: &text, pos: 0 };
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

    /// InternalizeJSONProperty：先自底向上处理子项，再调 `reviver.call(holder, key, value)`；给 `undefined` 就删掉。
    fn revive<'gc>(a: &mut Agent<'gc, '_>, holder: Value<'gc>, key: JsString<'gc>, reviver: Value<'gc>) -> Result<Value<'gc>, Error> {
        let v = a.get_named(holder, &key)?;
        if let Value::Object(o) = v {
            let keys = Writer::keys(o);
            for k in keys {
                let k = a.string(&k);
                let nv = Self::revive(a, v, k, reviver)?;
                if nv == Value::Undefined {
                    a.delete(v, &k)?;
                } else {
                    a.create_data_property(o, r8framework::Key::Str(&k), nv)?;
                }
            }
        }
        a.call(reviver, holder, &[key.into(), v])
    }

    /// `JSON.stringify(value, replacer, space)`。
    fn stringify<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (replacer, allow) = match args.at(1) {
            f if a.is_callable(f) => (Some(f), None),
            Value::Object(o) if matches!(o.borrow().kind, ObjectKind::Array(_)) => {
                let items = a.to_list(Value::Object(o))?;
                let mut keys: Vec<JsString<'gc>> = Vec::new();
                for item in items {
                    let k = match item {
                        Value::String(s) => Some(s),
                        Value::Number(_) => Some(a.to_string(item)?),
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
        let gap = match args.at(2) {
            Value::Number(n) => {
                #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "已夹到 [0, MAX_GAP]")]
                let n = libm::trunc(n.clamp(0.0, 10.0)) as usize;
                " ".repeat(n.min(MAX_GAP))
            }
            Value::String(s) => s.chars().take(MAX_GAP).collect(),
            _ => String::new(),
        };
        let mut w = Writer { gap, indent: String::new(), allow, replacer, stack: Vec::new() };
        let wrapper = ObjectData { proto: Some(a.realm.object_proto), ..ObjectData::default() }.alloc(a.mc);
        wrapper.borrow_mut(a.mc).set("", args.at(0));
        let key = SmallStr::from("");
        let mut out = String::new();
        if w.property(a, wrapper.into(), &key, &mut out)? { Ok(a.string(&out).into()) } else { Ok(Value::Undefined) }
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
