use alloc::string::String;
use alloc::vec::Vec;

use r8frame::{Agent, Error, Value};

use crate::{Args, Installer};

/// 全局函数与 `console`。
struct Global;

impl Global {
    /// `console.log(...)`：ToString 后空格连接交给平台输出。
    fn log<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let parts = args.iter().map(|&v| a.to_string(v).map(|s| String::from(&**s))).collect::<Result<Vec<_>, _>>()?;
        a.platform.print(&parts.join(" "));
        Ok(Value::Undefined)
    }

    fn is_nan<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(a.to_number(args.at(0))?.is_nan().into())
    }

    fn is_finite<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(a.to_number(args.at(0))?.is_finite().into())
    }

    /// `parseInt(s, radix)`：跳过前导空白，可带符号与 `0x`，读到第一个非数字为止。
    fn parse_int<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        /// 缺省进制
        const DECIMAL: u32 = 10;
        /// `0x` 前缀的进制
        const HEX: u32 = 16;
        let s = a.to_string(args.at(0))?;
        let mut t = s.trim_start();
        let negative = t.starts_with('-');
        if t.starts_with(['-', '+']) {
            t = &t[1..];
        }
        let mut radix = match args.at(1) {
            Value::Undefined => 0,
            r => a.to_int32(r)?.cast_unsigned(),
        };
        if (radix == 0 || radix == HEX) && (t.starts_with("0x") || t.starts_with("0X")) {
            t = &t[2..];
            radix = HEX;
        }
        if radix == 0 {
            radix = DECIMAL;
        }
        if !(2..=36).contains(&radix) {
            return Ok(f64::NAN.into());
        }
        let digits: Vec<u32> = t.chars().map_while(|c| c.to_digit(radix)).collect();
        if digits.is_empty() {
            return Ok(f64::NAN.into());
        }
        let n = digits.iter().fold(0.0, |acc, &d| acc * f64::from(radix) + f64::from(d));
        Ok((if negative { -n } else { n }).into())
    }

    /// `parseFloat(s)`：最长的十进制数前缀。
    fn parse_float<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let s = a.to_string(args.at(0))?;
        let t = s.trim_start();
        for (inf, x) in [("Infinity", f64::INFINITY), ("+Infinity", f64::INFINITY), ("-Infinity", f64::NEG_INFINITY)] {
            if t.starts_with(inf) {
                return Ok(x.into());
            }
        }
        let end = (1..=t.len()).rev().find(|&i| t.is_char_boundary(i) && t[..i].bytes().all(|c| c.is_ascii_digit() || b"+-.eE".contains(&c)) && t[..i].parse::<f64>().is_ok());
        Ok(end.map_or(f64::NAN, |i| t[..i].parse().unwrap_or(f64::NAN)).into())
    }
}

impl Installer<'_, '_> {
    pub(crate) fn global(&mut self) {
        let g = self.realm.global;
        self.set(g, "globalThis", g.into());
        // 值属性：不可写、不可枚举、不可配置
        for (name, v) in [("undefined", Value::Undefined), ("NaN", f64::NAN.into()), ("Infinity", f64::INFINITY.into())] {
            g.borrow_mut(self.mc).define(name, r8frame::Property { slot: r8frame::Slot::Data(v), enumerable: false, writable: false, configurable: false });
        }
        let console = self.plain();
        self.method(console, "log", Global::log, 0);
        self.set(g, "console", console.into());
        self.method(g, "isNaN", Global::is_nan, 1);
        self.method(g, "isFinite", Global::is_finite, 1);
        self.method(g, "parseInt", Global::parse_int, 2);
        self.method(g, "parseFloat", Global::parse_float, 1);
    }
}
