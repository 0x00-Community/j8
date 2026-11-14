//! Parallelism for built-in functions: with large data, plain-data work is split into chunks on the platform's thread
//! pool (fork–join; returns only when all is done).
//!
//! Only the parts that don't touch the GC heap or call user code (`Value` carries the `'gc` brand and can't cross
//! threads): the caller first extracts the keys to sort or the numbers to convert into plain data, computes in
//! parallel, then writes the results back.
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::cmp::Ordering;

use r8gc::{Job, Pool};

/// Below this many elements, do the work on the calling thread (the cost of splitting and joining isn't worth it)
const PAR_MIN: usize = 1 << 14;
/// Chunks per thread (smaller chunks, so threads that finish first pick up more)
const CHUNKS_PER_THREAD: usize = 2;

/// Borrow the platform's thread pool for parallel work.
#[derive(Clone, Copy)]
pub struct Par<'p>(pub &'p dyn Pool);

impl Par<'_> {
    /// How many chunks to split into
    fn chunk(self, len: usize) -> usize {
        len.div_ceil(self.0.threads() * CHUNKS_PER_THREAD).max(1)
    }

    fn worth(self, len: usize) -> bool {
        len >= PAR_MIN && self.0.threads() > 1
    }

    /// `out[i] = f(&input[i])`; large inputs are split into chunks in parallel
    pub fn map<T: Sync, R: Send + Default>(self, input: &[T], f: &(dyn Fn(&T) -> R + Sync)) -> Vec<R> {
        let mut out: Vec<R> = core::iter::repeat_with(R::default).take(input.len()).collect();
        if !self.worth(input.len()) {
            out.iter_mut().zip(input).for_each(|(o, x)| *o = f(x));
            return out;
        }
        let size = self.chunk(input.len());
        let jobs: Vec<Job<'_>> = out
            .chunks_mut(size)
            .zip(input.chunks(size))
            .map(|(o, i)| Box::new(move || o.iter_mut().zip(i).for_each(|(o, x)| *o = f(x))) as Job<'_>)
            .collect();
        self.0.run(jobs);
        out
    }

    /// Process by chunk: `f` gets a contiguous chunk, results are given in chunk order (small inputs are one whole
    /// chunk)
    pub fn chunks<T: Sync, R: Send + Default>(self, input: &[T], f: &(dyn Fn(&[T]) -> R + Sync)) -> Vec<R> {
        if !self.worth(input.len()) {
            return alloc::vec![f(input)];
        }
        let size = self.chunk(input.len());
        let mut out: Vec<R> = core::iter::repeat_with(R::default).take(input.len().div_ceil(size)).collect();
        let jobs: Vec<Job<'_>> = out.iter_mut().zip(input.chunks(size)).map(|(o, c)| Box::new(move || *o = f(c)) as Job<'_>).collect();
        self.0.run(jobs);
        out
    }

    /// Process chunks in place: chunk lengths are multiples of `unit` (e.g. the element byte size); large inputs are
    /// split into chunks in parallel
    pub fn each_mut<T: Send>(self, v: &mut [T], unit: usize, f: &(dyn Fn(&mut [T]) + Sync)) {
        let unit = unit.max(1);
        if !self.worth(v.len() / unit) {
            f(v);
            return;
        }
        let size = self.chunk(v.len() / unit) * unit;
        let jobs: Vec<Job<'_>> = v.chunks_mut(size).map(|c| Box::new(move || f(c)) as Job<'_>).collect();
        self.0.run(jobs);
    }

    /// The first position in `[0, n)` that matches: `f` checks a range and returns the first match in it; ranges are
    /// checked in parallel and the smallest is taken
    pub fn find_first(self, n: usize, f: &(dyn Fn(core::ops::Range<usize>) -> Option<usize> + Sync)) -> Option<usize> {
        if !self.worth(n) {
            return f(0..n);
        }
        let size = self.chunk(n);
        let mut found: Vec<Option<usize>> = (0..n).step_by(size).map(|_| None).collect();
        let jobs: Vec<Job<'_>> = found.iter_mut().zip((0..n).step_by(size)).map(|(o, s)| Box::new(move || *o = f(s..(s + size).min(n))) as Job<'_>).collect();
        self.0.run(jobs);
        found.into_iter().flatten().min()
    }

    /// Find the byte position of the first occurrence of `needle` in `hay`: from 1 MiB up, split into overlapping
    /// chunks searched in parallel (single-threaded search itself is already fast)
    pub fn find_str(self, hay: &str, needle: &str) -> Option<usize> {
        /// Search directly when the string is shorter than this
        const PAR_FIND_MIN: usize = 1 << 20;
        if hay.len() < PAR_FIND_MIN || self.0.threads() == 1 || needle.is_empty() {
            return hay.find(needle);
        }
        let size = hay.len().div_ceil(self.0.threads() * CHUNKS_PER_THREAD);
        // Move chunk boundaries onto character boundaries
        let boundary = |mut i: usize| {
            while i < hay.len() && !hay.is_char_boundary(i) {
                i += 1;
            }
            i.min(hay.len())
        };
        let starts: Vec<usize> = (0..hay.len()).step_by(size).map(boundary).collect();
        let mut found: Vec<Option<usize>> = starts.iter().map(|_| None).collect();
        let jobs: Vec<Job<'_>> = found
            .iter_mut()
            .zip(&starts)
            .map(|(o, &s)| {
                Box::new(move || {
                    // Look needle.len() − 1 bytes past the chunk end: a match spanning chunks belongs to the chunk
                    // where it starts
                    let end = boundary((s + size + needle.len() - 1).min(hay.len()));
                    *o = hay[s..end].find(needle).filter(|&i| i < size).map(|i| s + i);
                }) as Job<'_>
            })
            .collect();
        self.0.run(jobs);
        found.into_iter().flatten().min()
    }

    /// Stable sort: sort each chunk separately, then merge pairwise in parallel, round after round
    pub fn sort_by<T: Copy + Send + Sync>(self, v: &mut [T], cmp: &(dyn Fn(&T, &T) -> Ordering + Sync)) {
        if !self.worth(v.len()) {
            v.sort_by(cmp);
            return;
        }
        let size = self.chunk(v.len());
        let jobs: Vec<Job<'_>> = v.chunks_mut(size).map(|c| Box::new(move || c.sort_by(cmp)) as Job<'_>).collect();
        self.0.run(jobs);
        let mut buf = v.to_vec();
        let (mut src, mut dst): (&mut [T], &mut [T]) = (v, &mut buf);
        let mut width = size;
        let mut in_buf = false;
        while width < src.len() {
            let jobs: Vec<Job<'_>> = src
                .chunks(2 * width)
                .zip(dst.chunks_mut(2 * width))
                .map(|(s, d)| {
                    Box::new(move || {
                        let (l, r) = s.split_at(width.min(s.len()));
                        Self::merge(l, r, d, cmp);
                    }) as Job<'_>
                })
                .collect();
            self.0.run(jobs);
            core::mem::swap(&mut src, &mut dst);
            in_buf = !in_buf;
            width *= 2;
        }
        // The result ended up in the buffer: copy it back
        if in_buf {
            dst.copy_from_slice(src);
        }
    }

    /// Merge two sorted runs into `out` (on ties the left one comes first: stable)
    fn merge<T: Copy>(l: &[T], r: &[T], out: &mut [T], cmp: &(dyn Fn(&T, &T) -> Ordering + Sync)) {
        let (mut i, mut j) = (0, 0);
        for o in out.iter_mut() {
            *o = if j >= r.len() || (i < l.len() && cmp(&l[i], &r[j]) != Ordering::Greater) {
                i += 1;
                l[i - 1]
            } else {
                j += 1;
                r[j - 1]
            };
        }
    }
}
