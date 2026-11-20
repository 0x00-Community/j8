//! A second engine for the patterns the `regex` crate matches exactly like ECMAScript does on an all-ASCII subject
//! (finite automata with a literal prefilter instead of backtracking). [`Dfa::new`] translates the pattern and refuses
//! everything whose meaning could differ: back references, lookaround, named groups, `v` mode, anchors under `m`,
//! capture groups inside or under a quantifier (ECMAScript resets them on every iteration), Annex B oddities
//! (quantifiers without an atom, `{` that is not a quantifier, identity escapes of letters), non-ASCII pattern text.
//! regress stays the engine for the rest and for non-ASCII subjects.
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use core::ops::Range;

/// The translated pattern, compiled; `groups` is the number of capture groups.
pub(crate) struct Dfa {
    re: regex::bytes::Regex,
    groups: usize,
}

/// What a class atom is.
enum ClassAtom {
    Unit(u8),
    /// `\d` / `\w` / `\s`, already written out as class contents
    Set,
}

/// A match: whole range and each capture group's range.
type Match = (Range<usize>, Vec<Option<Range<usize>>>);

/// One open group (the whole pattern is the outermost) while translating.
#[expect(clippy::struct_excessive_bools, reason = "independent facts about one group")]
struct Open {
    capturing: bool,
    /// A capture group is somewhere inside
    captures: bool,
    /// Some finished alternative can match the empty string
    alt: bool,
    /// The items of this alternative before the last one can all match empty
    done: bool,
    /// The last item can match empty
    last: bool,
}

impl Open {
    fn new(capturing: bool) -> Self {
        Self { capturing, captures: false, alt: false, done: true, last: true }
    }

    fn item(&mut self, nullable: bool) {
        self.done &= self.last;
        self.last = nullable;
    }

    fn alternative(&mut self) {
        self.alt |= self.done && self.last;
        self.done = true;
        self.last = true;
    }

    fn nullable(&self) -> bool {
        self.alt || (self.done && self.last)
    }
}

/// ECMAScript pattern ⇒ `regex` syntax, over the pattern's bytes (all ASCII, checked on the way).
struct Translate<'s> {
    src: &'s [u8],
    at: usize,
    out: String,
    open: Vec<Open>,
    groups: usize,
    /// The last thing written can take a quantifier
    atom: bool,
    /// `m` flag: anchors would be line anchors with ECMAScript's four line terminators
    multiline: bool,
    dot_all: bool,
}

/// ECMAScript `\d` / `\w` / `\s` restricted to ASCII (the subject is ASCII), as class contents.
const DIGIT: &str = "0-9";
const WORD: &str = "0-9A-Za-z_";
const SPACE: &str = "\\t\\n\\x0B\\x0C\\r ";

/// Repetition counts above this are left to regress (the automaton would grow with them).
const MAX_REPEAT: u32 = 1000;

impl Dfa {
    /// `None` when the pattern / flags are outside what is translated exactly.
    pub(crate) fn new(source: &str, flags: &str) -> Option<Self> {
        if flags.contains('v') || !source.is_ascii() {
            return None;
        }
        let mut t = Translate { src: source.as_bytes(), at: 0, out: String::with_capacity(source.len() * 2), open: alloc::vec![Open::new(false)], groups: 0, atom: false, multiline: flags.contains('m'), dot_all: flags.contains('s') };
        t.pattern()?;
        let re = regex::bytes::RegexBuilder::new(&t.out).unicode(false).case_insensitive(flags.contains('i')).build().ok()?;
        (re.captures_len() == t.groups + 1).then_some(Self { re, groups: t.groups })
    }

    /// The first match at or after `start` in the ASCII text `s`: whole match and each group (byte = code unit index).
    pub(crate) fn find(&self, s: &str, start: usize) -> Option<Match> {
        if self.groups == 0 {
            return self.re.find_at(s.as_bytes(), start).map(|m| (m.range(), Vec::new()));
        }
        let c = self.re.captures_at(s.as_bytes(), start)?;
        let whole = c.get(0)?.range();
        Some((whole, (1..=self.groups).map(|i| c.get(i).map(|m| m.range())).collect()))
    }
}

impl Translate<'_> {
    fn peek(&self) -> Option<u8> {
        self.src.get(self.at).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.at += 1;
        Some(c)
    }

    fn eat(&mut self, c: u8) -> bool {
        let hit = self.peek() == Some(c);
        if hit {
            self.at += 1;
        }
        hit
    }

    fn literal(&mut self, c: u8) {
        let _ = write!(self.out, "\\x{{{c:02X}}}");
    }

    fn pattern(&mut self) -> Option<()> {
        while let Some(c) = self.next() {
            match c {
                b'(' => self.group()?,
                b')' => self.close()?,
                b'|' => {
                    self.out.push('|');
                    self.top()?.alternative();
                    self.atom = false;
                }
                b'*' | b'+' | b'?' => {
                    self.quantifier(c != b'+')?;
                    self.out.push(char::from(c));
                    self.lazy();
                }
                b'{' => self.braces()?,
                b'^' | b'$' if self.multiline => return None,
                b'^' | b'$' => {
                    self.out.push(char::from(c));
                    self.top()?.item(true);
                    self.atom = false;
                }
                b'.' => {
                    self.out.push_str(if self.dot_all { "(?s:.)" } else { "[^\\n\\r]" });
                    self.atom()?;
                }
                b'[' => self.class()?,
                b'\\' => self.escape()?,
                _ => {
                    self.literal(c);
                    self.atom()?;
                }
            }
        }
        (self.open.len() == 1).then_some(())
    }

    /// The innermost open group
    fn top(&mut self) -> Option<&mut Open> {
        self.open.last_mut()
    }

    /// An item that cannot match empty (a character or class)
    fn atom(&mut self) -> Option<()> {
        self.top()?.item(false);
        self.atom = true;
        Some(())
    }

    fn group(&mut self) -> Option<()> {
        let capturing = if self.eat(b'?') {
            if !self.eat(b':') {
                return None;
            }
            self.out.push_str("(?:");
            false
        } else {
            self.out.push('(');
            self.groups += 1;
            self.open.iter_mut().for_each(|o| o.captures = true);
            true
        };
        self.open.push(Open::new(capturing));
        self.atom = false;
        Some(())
    }

    fn close(&mut self) -> Option<()> {
        if self.open.len() < 2 {
            return None;
        }
        let g = self.open.pop()?;
        self.out.push(')');
        let quantified = match self.peek() {
            Some(b'*' | b'+' | b'?') => true,
            Some(b'{') => self.src.get(self.at + 1).is_some_and(u8::is_ascii_digit),
            _ => false,
        };
        // ECMAScript fails an iteration that matched empty and backtracks into it; `regex` takes it
        if quantified && (g.capturing || g.captures || g.nullable()) {
            return None;
        }
        self.top()?.item(g.nullable());
        self.atom = true;
        Some(())
    }

    /// A quantifier needs an atom right before it (no quantifier after a quantifier, an assertion or nothing);
    /// `optional`: it may repeat zero times
    fn quantifier(&mut self, optional: bool) -> Option<()> {
        self.atom.then_some(())?;
        self.atom = false;
        if optional {
            self.top()?.last = true;
        }
        Some(())
    }

    fn lazy(&mut self) {
        if self.eat(b'?') {
            self.out.push('?');
        }
    }

    fn number(&mut self) -> Option<u32> {
        let from = self.at;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.at += 1;
        }
        core::str::from_utf8(self.src.get(from..self.at)?).ok()?.parse().ok().filter(|&n| n <= MAX_REPEAT)
    }

    /// `{n}` / `{n,}` / `{n,m}`; anything else (a literal `{` in Annex B) is refused
    fn braces(&mut self) -> Option<()> {
        let lo = self.number()?;
        self.quantifier(lo == 0)?;
        let hi = if self.eat(b',') { if self.peek() == Some(b'}') { None } else { Some(self.number()?) } } else { Some(lo) };
        if !self.eat(b'}') || hi.is_some_and(|h| h < lo) {
            return None;
        }
        let _ = match hi {
            Some(h) if h == lo => write!(self.out, "{{{lo}}}"),
            Some(h) => write!(self.out, "{{{lo},{h}}}"),
            None => write!(self.out, "{{{lo},}}"),
        };
        self.lazy();
        Some(())
    }

    /// A character escape's code unit (`\t`, `\x41`, `A`, `\0`, escaped punctuation); `None` for anything else
    fn char_escape(&mut self, c: u8) -> Option<u8> {
        match c {
            b't' => Some(b'\t'),
            b'n' => Some(b'\n'),
            b'r' => Some(b'\r'),
            b'v' => Some(0x0B),
            b'f' => Some(0x0C),
            b'0' if !self.peek().is_some_and(|d| d.is_ascii_digit()) => Some(0),
            b'x' => self.hex(2),
            b'u' => self.hex(4),
            c if c.is_ascii_punctuation() => Some(c),
            _ => None,
        }
    }

    fn hex(&mut self, n: usize) -> Option<u8> {
        let digits = core::str::from_utf8(self.src.get(self.at..self.at + n)?).ok()?;
        let v = u32::from_str_radix(digits, 16).ok().filter(|_| digits.bytes().all(|d| d.is_ascii_hexdigit()))?;
        self.at += n;
        u8::try_from(v).ok().filter(u8::is_ascii)
    }

    fn escape(&mut self) -> Option<()> {
        let c = self.next()?;
        let (set, negated) = match c {
            b'd' => (DIGIT, false),
            b'D' => (DIGIT, true),
            b'w' => (WORD, false),
            b'W' => (WORD, true),
            b's' => (SPACE, false),
            b'S' => (SPACE, true),
            b'b' | b'B' => {
                self.out.push('\\');
                self.out.push(char::from(c));
                self.top()?.item(true);
                self.atom = false;
                return Some(());
            }
            _ => {
                let u = self.char_escape(c)?;
                self.literal(u);
                return self.atom();
            }
        };
        let _ = write!(self.out, "[{}{set}]", if negated { "^" } else { "" });
        self.atom()
    }

    /// One class atom
    fn class_atom(&mut self) -> Option<ClassAtom> {
        match self.next()? {
            b'\\' => match self.next()? {
                c @ (b'd' | b'w' | b's') => {
                    self.out.push_str(match c {
                        b'd' => DIGIT,
                        b'w' => WORD,
                        _ => SPACE,
                    });
                    Some(ClassAtom::Set)
                }
                b'b' => Some(ClassAtom::Unit(0x08)),
                c => self.char_escape(c).map(ClassAtom::Unit),
            },
            c => Some(ClassAtom::Unit(c)),
        }
    }

    fn class(&mut self) -> Option<()> {
        self.out.push('[');
        if self.eat(b'^') {
            self.out.push('^');
        }
        if self.peek() == Some(b']') {
            return None;
        }
        while !self.eat(b']') {
            let ClassAtom::Unit(lo) = self.class_atom()? else {
                // `\d-x` is Annex B's literal `-`: refused
                if self.peek() == Some(b'-') && self.src.get(self.at + 1) != Some(&b']') {
                    return None;
                }
                continue;
            };
            if self.peek() == Some(b'-') && self.src.get(self.at + 1).is_some_and(|&c| c != b']') {
                self.at += 1;
                let ClassAtom::Unit(hi) = self.class_atom()? else { return None };
                if hi < lo {
                    return None;
                }
                self.literal(lo);
                self.out.push('-');
                self.literal(hi);
            } else {
                self.literal(lo);
            }
        }
        self.out.push(']');
        self.atom()
    }
}

