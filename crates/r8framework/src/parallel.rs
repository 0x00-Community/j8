//! 内建函数的并行：数据量大时把纯数据的活切块放到平台的线程池上（分叉—汇合，跑完才返回）。
//!
//! 只做不碰 GC 堆、不调用户代码的部分（`Value` 带 `'gc` 品牌，不能跨线程）：调用方先把要排序的键、要转换的数
//! 抽成普通数据，并行算完再写回。
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::cmp::Ordering;

use r8gc::{Job, Pool};

/// 元素少于这么多就在调用线程上做（切块与汇合的开销不划算）
const PAR_MIN: usize = 1 << 14;
/// 每个线程分几块（块小一点，先做完的线程接着拿）
const CHUNKS_PER_THREAD: usize = 2;

/// 借平台的线程池做并行活。
#[derive(Clone, Copy)]
pub struct Par<'p>(pub &'p dyn Pool);

impl Par<'_> {
    /// 切成几块
    fn chunk(self, len: usize) -> usize {
        len.div_ceil(self.0.threads() * CHUNKS_PER_THREAD).max(1)
    }

    fn worth(self, len: usize) -> bool {
        len >= PAR_MIN && self.0.threads() > 1
    }

    /// `out[i] = f(&input[i])`，大的切块并行
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

    /// 按块处理：`f` 拿到连续的一块，结果按块的顺序给出（小的整个一块）
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

    /// 稳定排序：各块分别排好，再一轮轮两两并行归并
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
        // 结果停在缓冲里：抄回去
        if in_buf {
            dst.copy_from_slice(src);
        }
    }

    /// 两段排好的归并进 `out`（相等时左边的在前：稳定）
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
