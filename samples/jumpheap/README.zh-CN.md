# jumpheap

[English](README.md) · **简体中文**

几个常驻的 j8 worker 分在不同进程里，JS 对象互相引用对方的堆。每个进程有一个
[paragc](https://crates.io/crates/paragc) 公开堆（`/dev/shm` 里的内存文件）；它的 worker（[workerd](../workerd)
式的 Worker，`export default { fetch }`）在那里建 JS 矩阵，通过 HTTP 发给别的 worker，也监听别人发来的。收到的是一个
`object` 事件，带着一个 JS `Matrix`：对象在接收方自己的 isolate 堆里，各行是指着发送方堆的 `Float64Array`，不是拷贝。
在它上面的所有计算都是普通 JS。

```sh
cargo run --release -- examples/matrix.js --name wa --dir /dev/shm/jumpheap   # 打印端口
```

## JS 接口

| | |
|---|---|
| `await heaps.matrix(rows)` | 本进程堆里新建 `rows` × 100000 的全零 `Matrix` |
| `matrix.data[i]` | 第 `i` 行：公开堆内存（本进程的或别的进程的）上的 `Float64Array` |
| `matrix.rows`、`matrix.cols`、`matrix.local` | 形状；是否在本进程堆里 |
| `await heaps.send(url, matrix, name, task)` | 把 `matrix` 交给 `url` 处的 worker，得到它的回应 |
| `heaps.addEventListener('object', e => …)` | 收到矩阵：`e.object`、`e.name`、`e.task`；`e.respondWith(value)` 回给发送方 |

收到的矩阵在发送方堆里占一个入口，只要接收方还有它任何一行的 buffer，各行就活着；发送方进程死后，它写进别的堆的内容还在。

## 三 worker 测试

`tests/three.rs` 起 wa、wb、wc，各有矩阵 a、b、c（在 JS 里填）。wc 把 c 发给 wa，wa 算 a·c；wa 把 a 发给 wb，wb 把 b 的前一半
写到 a 上然后死掉；wc 再发一次 c，a·c 与 a 的总和必须是被覆盖一半之后的 a 的值。

```sh
cargo test --release                            # 40 × 100000
JUMPHEAP_ROWS=10000 cargo test --release -- --nocapture   # 完整 10000 × 100000（/dev/shm 里每个矩阵 8 GB）
```
