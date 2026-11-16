//! Collection time versus thread count: full and minor collections on a large live heap (wide tree + chains).
//! `cargo run --release --example bench -- <threads>`
use std::num::NonZeroUsize;
use std::time::Instant;

use moremath::ThreadMulti;
use j8gc::{Collect, Gc, J8Heap, RefLock, Rootable};

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

/// Number of chains and the length of each: 4 million nodes in total
const CHAINS: usize = 1 << 14;
const LENGTH: usize = 256;

fn main() {
    let threads = std::env::args().nth(1).and_then(|s| s.parse().ok()).and_then(NonZeroUsize::new).unwrap_or(NonZeroUsize::MIN);
    let pool = ThreadMulti::from(threads);
    let mut heap: J8Heap<'_, Rootable![Root<'_>]> = J8Heap::new(&pool, |_| Root { chains: Vec::new() });
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
        // Drop half of the chains each round: the old generation becomes sparse, triggering compaction
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
