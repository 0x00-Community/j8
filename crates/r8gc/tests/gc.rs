//! 回收器的确定事实：活的留着、值不变；死的析构恰好一次；老对象经写屏障收进的新对象活着；稀疏的页会被压缩。
use std::cell::Cell;
use std::rc::Rc;

use moremath::ThreadMulti;
use r8gc::{Collect, DynamicRootSet, Gc, Pool, R8Heap, RefLock, Rootable};

/// 析构时计数
#[derive(Collect)]
#[collect(require_static)]
struct Counted(Rc<Cell<usize>>);

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

#[derive(Collect)]
#[collect(no_drop)]
struct Node<'gc> {
    value: usize,
    next: Option<Gc<'gc, RefLock<Node<'gc>>>>,
    payload: Vec<u64>,
}

type List<'gc> = Gc<'gc, RefLock<Node<'gc>>>;

#[derive(Collect)]
#[collect(no_drop)]
struct Root<'gc> {
    list: Option<List<'gc>>,
    keep: Vec<Gc<'gc, Counted>>,
    nodes: Vec<List<'gc>>,
    roots: DynamicRootSet<'gc>,
}

fn heap(pool: &dyn Pool) -> R8Heap<'_, Rootable![Root<'_>]> {
    R8Heap::new(pool, |mc| Root { list: None, keep: Vec::new(), nodes: Vec::new(), roots: DynamicRootSet::new(mc) })
}

/// 链表的值依次是 n-1 … 0，每个节点带 4 个 u64
fn sum(list: Option<List<'_>>) -> (usize, usize) {
    let (mut count, mut total, mut at) = (0, 0, list);
    while let Some(n) = at {
        let node = n.borrow();
        assert_eq!(node.payload, vec![node.value as u64; 4]);
        (count, total, at) = (count + 1, total + node.value, node.next);
    }
    (count, total)
}

#[test]
fn survives_minor_major_and_compaction() {
    const N: usize = 20_000;
    let pool = ThreadMulti::default();
    let mut a = heap(&pool);
    a.mutate_root(|mc, root| {
        let mut head = None;
        for value in 0..N {
            head = Some(Gc::new(mc, RefLock::new(Node { value, next: head, payload: vec![value as u64; 4] })));
            // 垃圾：留下稀疏的页
            for _ in 0..9 {
                Gc::new(mc, RefLock::new(Node { value, next: None, payload: Vec::new() }));
            }
        }
        root.list = head;
    });
    for _ in 0..3 {
        a.collect_all();
        assert_eq!(a.mutate(|_, root| sum(root.list)), (N, N * (N - 1) / 2));
    }
    assert!(a.metrics().minor_count() >= 1 && a.metrics().major_count() == 3);
}

#[test]
fn drops_dead_exactly_once() {
    let dropped = Rc::new(Cell::new(0));
    let pool = ThreadMulti::default();
    let mut a = heap(&pool);
    a.mutate_root(|mc, root| {
        for i in 0..1000 {
            let c = Gc::new(mc, Counted(dropped.clone()));
            if i % 2 == 0 {
                root.keep.push(c);
            }
        }
    });
    a.collect_all();
    assert_eq!(dropped.get(), 500);
    a.collect_all();
    assert_eq!(dropped.get(), 500);
    drop(a);
    assert_eq!(dropped.get(), 1000);
}

#[test]
fn barrier_keeps_young_reachable_from_old() {
    let pool = ThreadMulti::default();
    let mut a = heap(&pool);
    a.mutate_root(|mc, root| root.list = Some(Gc::new(mc, RefLock::new(Node { value: 1, next: None, payload: vec![1; 4] }))));
    // 头节点进老年代
    a.collect_all();
    a.mutate(|mc, root| {
        let head = root.list.expect("head");
        head.borrow_mut(mc).next = Some(Gc::new(mc, RefLock::new(Node { value: 2, next: None, payload: vec![2; 4] })));
    });
    // 只做次回收：新节点只能靠记忆集找到
    a.collect_debt();
    a.mutate(|mc, _| {
        for _ in 0..(5 << 20) / 32 {
            Gc::new(mc, [0u64; 4]);
        }
    });
    a.collect_debt();
    assert!(a.metrics().minor_count() >= 2);
    assert_eq!(a.mutate(|_, root| sum(root.list)), (2, 3));
}

#[test]
fn compacts_sparse_pages() {
    const N: usize = 50_000;
    let pool = ThreadMulti::default();
    let mut a = heap(&pool);
    a.mutate_root(|mc, root| {
        root.nodes = (0..N).map(|value| Gc::new(mc, RefLock::new(Node { value, next: None, payload: vec![value as u64; 4] }))).collect();
    });
    // 全部进老年代，再放掉 15/16：页变稀疏
    a.collect_all();
    a.mutate_root(|_, root| root.nodes.retain(|n| n.borrow().value % 16 == 0));
    a.collect_all();
    assert!(a.metrics().compaction_count() >= 1);
    let values: Vec<usize> = a.mutate(|_, root| {
        root.nodes
            .iter()
            .map(|n| {
                let n = n.borrow();
                assert_eq!(n.payload, vec![n.value as u64; 4]);
                n.value
            })
            .collect()
    });
    assert_eq!(values, (0..N).step_by(16).collect::<Vec<_>>());
}

#[test]
fn dynamic_roots_survive() {
    let pool = ThreadMulti::default();
    let mut a = heap(&pool);
    let handle = a.mutate(|mc, root| root.roots.stash::<Rootable![Node<'_>]>(mc, Gc::new(mc, Node { value: 7, next: None, payload: vec![7; 4] })));
    for _ in 0..3 {
        a.collect_all();
    }
    assert_eq!(a.mutate(|_, root| root.roots.fetch(&handle).value), 7);
}

#[test]
fn heaps_share_one_pool() {
    let pool = ThreadMulti::default();
    let (mut a, mut b) = (heap(&pool), heap(&pool));
    for (h, base) in [(&mut a, 0), (&mut b, 1_000_000)] {
        h.mutate_root(|mc, root| {
            root.nodes = (0..10_000).map(|i| Gc::new(mc, RefLock::new(Node { value: base + i, next: None, payload: vec![(base + i) as u64; 4] }))).collect();
        });
    }
    for _ in 0..3 {
        a.collect_all();
        b.collect_all();
    }
    for (h, base) in [(&a, 0), (&b, 1_000_000)] {
        let values: Vec<usize> = h.mutate(|_, root| root.nodes.iter().map(|n| n.borrow().value).collect());
        assert_eq!(values, (base..base + 10_000).collect::<Vec<_>>());
    }
}

#[test]
fn wide_graph_marks_in_parallel() {
    const FANOUT: usize = 64;
    let pool = ThreadMulti::default();
    let mut a = heap(&pool);
    // 两层的宽树：64 × 64 个叶子，各带一条 16 节点的链
    a.mutate_root(|mc, root| {
        root.nodes = (0..FANOUT * FANOUT)
            .map(|i| {
                let mut head = None;
                for j in 0..16 {
                    head = Some(Gc::new(mc, RefLock::new(Node { value: i * 16 + j, next: head, payload: vec![(i * 16 + j) as u64; 4] })));
                }
                head.expect("chain")
            })
            .collect();
    });
    for _ in 0..3 {
        a.collect_all();
    }
    let total: usize = a.mutate(|_, root| root.nodes.iter().map(|&n| sum(Some(n)).1).sum());
    assert_eq!(total, (0..FANOUT * FANOUT * 16).sum::<usize>());
}

/// 析构计数用原子量：能在线程池上析构
struct Shared(std::sync::Arc<std::sync::atomic::AtomicUsize>);

// SAFETY: 没有 Gc；Arc 与原子量在哪个线程析构都行
unsafe impl Collect<'_> for Shared {
    const NEEDS_TRACE: bool = false;
    const PARALLEL_DROP: bool = true;
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// 根是一串 `Shared`
type SharedHeap<'p> = R8Heap<'p, Rootable![Vec<Gc<'_, Shared>>]>;

#[test]
fn parallel_drop_exactly_once() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    const { assert!(<Node<'_> as Collect>::PARALLEL_DROP && !<Counted as Collect>::PARALLEL_DROP) };
    let dropped = Arc::new(AtomicUsize::new(0));
    let pool = ThreadMulti::default();
    {
        let mut a: SharedHeap<'_> = R8Heap::new(&pool, |_| Vec::new());
        a.set_parallel_drop(true);
        a.mutate_root(|mc, keep| {
            for i in 0..100_000 {
                let s = Gc::new(mc, Shared(dropped.clone()));
                if i % 4 == 0 {
                    keep.push(s);
                }
            }
        });
        // 死在新生代的（次回收里并行析构）
        a.collect_all();
        assert_eq!(dropped.load(Ordering::Relaxed), 75_000);
        // 死在老年代的（清扫里并行析构）
        a.mutate_root(|_, keep| keep.truncate(5_000));
        a.collect_all();
        assert_eq!(dropped.load(Ordering::Relaxed), 95_000);
    }
    assert_eq!(dropped.load(Ordering::Relaxed), 100_000);
}
