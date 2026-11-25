use alloc::sync::Arc;
use alloc::vec::Vec;

use j8lockfree::OnceBox;

const MARK_EVERY: usize = 4096;

pub(crate) struct Script {
    text: Arc<str>,
    lines: OnceBox<Lines>,
}

struct Lines {
    starts: Vec<u32>,
    marks: Vec<(u32, u32)>,
}

impl Script {
    pub(crate) fn new(text: Arc<str>) -> Self {
        Self { text, lines: OnceBox::default() }
    }

    pub(crate) fn text(&self, range: core::ops::Range<u32>) -> Option<&str> {
        self.text.get(range.start as usize..range.end as usize)
    }

    fn lines(&self) -> &Lines {
        self.lines.get_or_init(|| Lines::of(&self.text))
    }

    pub(crate) fn locate(&self, at: u32) -> (u32, u32) {
        let lines = self.lines();
        let at = (at as usize).min(self.text.len());
        let line = lines.starts.partition_point(|&s| s as usize <= at);
        let start = lines.starts[line.saturating_sub(1)] as usize;
        let column = lines.units(&self.text, at) - lines.units(&self.text, start) + 1;
        (u32::try_from(line).unwrap_or(u32::MAX), u32::try_from(column).unwrap_or(u32::MAX))
    }

    pub(crate) fn offset(&self, line: u32, column: u32) -> u32 {
        let lines = self.lines();
        let i = line.max(1) as usize - 1;
        let Some(&start) = lines.starts.get(i) else { return u32::try_from(self.text.len()).unwrap_or(u32::MAX) };
        let start = start as usize;
        let end = lines.starts.get(i + 1).map_or(self.text.len(), |&s| s as usize);
        let want = lines.units(&self.text, start) + column.saturating_sub(1) as usize;
        let (mark, units) = lines.marks[lines.marks.partition_point(|&(_, u)| u as usize <= want) - 1];
        if mark as usize >= end {
            return u32::try_from(end).unwrap_or(u32::MAX);
        }
        let (mut at, mut units) = if (mark as usize) < start { (start, lines.units(&self.text, start)) } else { (mark as usize, units as usize) };
        for c in self.text[at..end].chars() {
            if units >= want {
                break;
            }
            units += c.len_utf16();
            at += c.len_utf8();
        }
        u32::try_from(at).unwrap_or(u32::MAX)
    }
}

impl Lines {
    fn of(text: &str) -> Self {
        let mut starts = alloc::vec![0];
        let mut marks = alloc::vec![(0, 0)];
        let mut units = 0u32;
        let mut next_mark = MARK_EVERY;
        let mut chars = text.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if i >= next_mark {
                marks.push((u32::try_from(i).unwrap_or(u32::MAX), units));
                next_mark = i + MARK_EVERY;
            }
            units += u32::try_from(c.len_utf16()).unwrap_or(2);
            let after = match c {
                '\r' if chars.peek().is_some_and(|&(_, n)| n == '\n') => continue,
                '\n' | '\r' | '\u{2028}' | '\u{2029}' => i + c.len_utf8(),
                _ => continue,
            };
            starts.push(u32::try_from(after).unwrap_or(u32::MAX));
        }
        Self { starts, marks }
    }

    fn units(&self, text: &str, at: usize) -> usize {
        let (mark, units) = self.marks[self.marks.partition_point(|&(m, _)| m as usize <= at) - 1];
        units as usize + text.get(mark as usize..at).map_or(0, |s| s.encode_utf16().count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_and_columns_count_utf16_units_across_checkpoints() {
        let mut text = alloc::string::String::from("a\r\nb\u{2028}c\n");
        text.push_str(&"x".repeat(5000));
        text.push_str("\u{1F600}yz\n");
        let script = Script::new(Arc::from(text.as_str()));
        let at = |needle: &str| u32::try_from(text.find(needle).expect("in the text")).expect("a short text");
        assert_eq!(script.locate(at("b")), (2, 1));
        assert_eq!(script.locate(at("c")), (3, 1));
        assert_eq!(script.locate(at("y")), (4, 5003));
        assert_eq!(script.offset(4, 5003), at("y"));
        assert_eq!(script.offset(2, 1), at("b"));
        assert_eq!(script.offset(9, 1), u32::try_from(text.len()).expect("a short text"));
    }
}
