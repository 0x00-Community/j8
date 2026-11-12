# r8gc

分代、可移动（压缩）、并行的 Rust GC，`'gc` 品牌生命周期，`no_std`（需要 `alloc`）。

- 新生代按块碰撞分配；次回收从根与记忆集出发，把活对象并行复制进老年代（写屏障维护记忆集）
- 老年代按尺寸档分页；主回收增量标记（纪元翻转、后向写屏障），标记 / 清扫 / 压缩都在线程池上并行
- 线程池是借来的 `Pool`：[moremath](https://crates.io/crates/moremath) 的 `ThreadMulti` / `ThreadSingle` 直接能用，几个 `R8Heap` 可共用一个
- 回收只在 `R8Heap::mutate` 之外发生，那时所有 `Gc` 都在根下面，所以对象能搬：`Collect` 除了 `trace` 还有 `relocate`

API 沿用 [gc-arena](https://github.com/kyren/gc-arena) 的形状（`Gc` / `Lock` / `RefLock` / `Rootable!` / `DynamicRootSet`）。

## 用法

```rust
use moremath::ThreadMulti;
use r8gc::{Collect, Gc, R8Heap, RefLock, Rootable};

#[derive(Collect)]
#[collect(no_drop)]
struct Node<'gc> {
    value: u64,
    next: Option<Gc<'gc, RefLock<Node<'gc>>>>,
}

#[derive(Collect)]
#[collect(no_drop)]
struct Root<'gc> {
    head: Option<Gc<'gc, RefLock<Node<'gc>>>>,
}

let pool = ThreadMulti::from(std::num::NonZeroUsize::new(4).unwrap());
let mut heap: R8Heap<'_, Rootable![Root<'_>]> = R8Heap::new(&pool, |_| Root { head: None });
heap.mutate_root(|mc, root| {
    for value in 0..1000 {
        root.head = Some(Gc::new(mc, RefLock::new(Node { value, next: root.head })));
    }
});
heap.collect_all();
```

回收耗时随线程数的变化见 `cargo run --release --example bench -- <线程数>`。

## Feature

- `verify`：每次回收后把整个老年代的边校验一遍（调试用，很慢）

## 许可

MIT。部分设计与代码来自 gc-arena，其许可见 `LICENSE-gc-arena`。
