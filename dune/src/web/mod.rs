mod http;
mod timer;

use std::io::Write;

use j8::frame::Agent;
use j8::Bytes;

pub use http::Http;
pub use timer::Timer;

const PRELUDE: &str = concat!(
    include_str!("console.js"),
    include_str!("events.js"),
    include_str!("timers.js"),
    include_str!("encoding.js"),
    include_str!("url.js"),
    include_str!("fetch.js"),
    include_str!("misc.js"),
);

pub struct Stdio;

#[j8::object("stdio")]
impl Stdio {
    fn out(text: &str) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
    }

    fn err(text: &str) {
        let _ = std::io::stderr().write_all(text.as_bytes());
    }

    fn is_tty(fd: u32) -> bool {
        use std::io::IsTerminal;
        match fd {
            0 => std::io::stdin().is_terminal(),
            1 => std::io::stdout().is_terminal(),
            _ => std::io::stderr().is_terminal(),
        }
    }

    fn exit(code: Option<i32>) {
        let _ = std::io::stdout().flush();
        std::process::exit(code.unwrap_or(0));
    }
}

pub struct Clock;

#[j8::object("clock")]
impl Clock {
    fn now(a: &mut Agent<'_, '_>) -> f64 {
        a.platform.now().as_secs_f64() * 1e3
    }

    fn wall(a: &mut Agent<'_, '_>) -> f64 {
        a.platform.wall_clock().as_secs_f64() * 1e3
    }
}

pub struct Codec;

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

#[j8::object("codec")]
impl Codec {
    fn utf8_encode(text: &str) -> Bytes {
        Bytes(text.as_bytes().to_vec())
    }

    fn utf8_decode(bytes: Bytes, fatal: Option<bool>) -> Option<String> {
        match String::from_utf8(bytes.0) {
            Ok(s) => Some(s),
            Err(_) if fatal == Some(true) => None,
            Err(e) => Some(String::from_utf8_lossy(e.as_bytes()).into_owned()),
        }
    }

    fn base64_encode(bytes: Bytes) -> String {
        let mut out = String::with_capacity(bytes.0.len().div_ceil(3) * 4);
        for chunk in bytes.0.chunks(3) {
            let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(char::from(BASE64[(n >> (18 - 6 * i) & 63) as usize]));
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    fn base64_decode(text: &str) -> Option<Bytes> {
        let mut out = Vec::with_capacity(text.len() / 4 * 3);
        let (mut acc, mut bits) = (0u32, 0);
        let body = text.trim_end_matches(|c: char| c == '=' || c.is_ascii_whitespace());
        for c in body.bytes().filter(|c| !c.is_ascii_whitespace()) {
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' | b'-' => 62,
                b'/' | b'_' => 63,
                _ => return None,
            };
            acc = acc << 6 | u32::from(v);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits & 0xff) as u8);
            }
        }
        (bits < 6).then_some(Bytes(out))
    }
}

pub struct Random;

#[j8::object("random")]
impl Random {
    fn bytes(n: usize) -> Bytes {
        let mut out = vec![0; n];
        getrandom::fill(&mut out).expect("the system random source");
        Bytes(out)
    }
}

j8::runtime! {
    pub Web {
        natives: [Stdio, Clock, Codec, Random, Timer, Http],
        prelude: PRELUDE,
    }
}
