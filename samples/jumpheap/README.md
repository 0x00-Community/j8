# jumpheap

**English** · [简体中文](README.zh-CN.md)

Resident j8 workers in separate processes whose JS objects reference each other's heaps. Each process owns a
[paragc](https://crates.io/crates/paragc) public heap, a memory file in `/dev/shm`; its worker (a
[workerd](../workerd)-style Worker, `export default { fetch }`) makes JS matrices whose rows live there, sends them to
other workers over HTTP, and listens for theirs. What arrives is an `object` event carrying a JS `Matrix` in the
receiver's own isolate heap whose rows are `Float64Array`s over the sender's heap: not a copy. Everything computed on
it is plain JS.

```sh
cargo run --release -- examples/matrix.js --name wa --dir /dev/shm/jumpheap   # prints its port
```

## JS API

| | |
|---|---|
| `await heaps.matrix(rows)` | a new `Matrix` of `rows` × 100000 zeros in this process's heap |
| `matrix.data[i]` | row `i`: a `Float64Array` over public-heap memory (this process's or another's) |
| `matrix.rows`, `matrix.cols`, `matrix.local` | its shape; whether it lives in this process's heap |
| `await heaps.send(url, matrix, name, task)` | hand `matrix` to the worker at `url`; gives what it responded |
| `heaps.addEventListener('object', e => …)` | a matrix arrived: `e.object`, `e.name`, `e.task`; `e.respondWith(value)` answers the sender |

A received matrix takes an entry in the sender's heap, which keeps its rows alive while any of their buffers lives in
the receiver; when the sender's process dies, what it wrote into other heaps stays.

## The three-worker test

`tests/three.rs` starts wa, wb and wc, each with matrix a, b, c (filled in JS). wc sends c to wa, which computes a·c;
wa sends a to wb, which writes the first half of b over it and dies; wc sends c again, and a·c and the sum of a must
be those of the half-overwritten a.

```sh
cargo test --release                            # 40 × 100000
JUMPHEAP_ROWS=10000 cargo test --release -- --nocapture   # the full 10000 × 100000 (8 GB per matrix in /dev/shm)
```
