//! 回收耗时随线程数的变化：大的活堆（宽树 + 链）上做完整回收与次回收。
//! `cargo run --release --example bench -- <线程数>`
use std::num::NonZeroUsize;
use std::time::Instant;

use moremath::ThreadMulti;
use r8gc::{Collect, Gc, R8Heap, RefLock, Rootable};

#[derive(Collect)]
#[collect(no_drop)]
struct Node<'gc> {
    value: u64,
    next: Option<Gc<'gc, RefLock<Node<'gc>>>>,
    payload: Vec<u64>,
}

#[derive(Collect)]
#[collect(no_drop)]
struct Root<'gc> {
    chains: Vec<Gc<'gc, RefLock<Node<'gc>>>>,
}

/// 链的条数与每条的长度：共 400 万个节点
const CHAINS: usize = 1 << 14;
const LENGTH: usize = 256;

fn main() {
    let threads = std::env::args().nth(1).and_then(|s| s.parse().ok()).and_then(NonZeroUsize::new).unwrap_or(NonZeroUsize::MIN);
    let pool = ThreadMulti::from(threads);
    let mut heap: R8Heap<'_, Rootable![Root<'_>]> = R8Heap::new(&pool, |_| Root { chains: Vec::new() });
    let t = Instant::now();
    heap.mutate_root(|mc, root| {
        root.chains = (0..CHAINS)
            .map(|c| {
                let mut chain = None;
                for i in 0..LENGTH {
                    let value = (c * LENGTH + i) as u64;
                    chain = Some(Gc::new(mc, RefLock::new(Node { value, next: chain, payload: vec![value; 2] })));
                }
                chain.expect("chain")
            })
            .collect();
    });
    let build = t.elapsed();
    let mut times = Vec::new();
    for round in 0..4 {
        // 每轮放掉一半的链：老年代变稀疏，触发压缩
        if round > 0 {
            heap.mutate_root(|_, root| {
                let keep = root.chains.len() / 2;
                root.chains.truncate(keep.max(1));
            });
        }
        let t = Instant::now();
        heap.collect_all();
        times.push(t.elapsed());
    }
    let sum: u64 = heap.mutate(|_, root| root.chains.iter().map(|n| n.borrow().value).sum());
    println!(
        "threads {threads}: build {build:.2?}, collect_all {:?} (minor {}, major {}, compact {}), check {sum}",
        times,
        heap.metrics().minor_count(),
        heap.metrics().major_count(),
        heap.metrics().compaction_count()
    );
}
