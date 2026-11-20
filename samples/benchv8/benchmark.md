# j8 vs. V8 performance

The same set of JS benchmarks (`bench/*.js`) runs on j8, V8 `--jitless` (interpreter only) and V8 (with JIT). V8 is embedded via rusty_v8 `=152.2.0`.

## Running

```text
cd samples/benchv8
cargo run --release                     # run all three engines once each and print the comparison table
cargo run --release -- --engine j8      # run only one: j8 / v8 / v8-jitless
cargo run --release -- --runs 10        # runs per benchmark, the shortest is kept (default 5)
```

- V8's `--jitless` is a process-wide flag, so each engine runs in its own child process.
- Timing covers compile + execute only; each run uses a fresh execution environment, and the shortest run is kept.
- Each script's completion value is a checksum that must match across the three engines (the check column in the table).
- benchv8 is a workspace of its own (excluded from the root workspace), so a normal `cargo build --workspace` doesn't pull V8's prebuilt library.

## Results

j8 runs with its JIT (`j8::jit::Jit`; `J8_NOJIT=1` runs the interpreter only). `--runs 10`, pinned to the performance
cores (`taskset -c 0-3`), load average below 1. All 11 benchmarks produce matching checksums on all three engines.

| Benchmark | j8 ms | V8 jitless ms | V8 ms | j8 / jitless | j8 / V8 |
|---|---:|---:|---:|---:|---:|
| fib | 1.2 | 9.0 | 1.3 | 0.1× | 0.9× |
| loop | 58.3 | 185.0 | 82.8 | 0.3× | 0.7× |
| objects | 0.9 | 13.6 | 2.5 | 0.1× | 0.4× |
| array | 40.8 | 13.0 | 6.5 | 3.1× | 6.3× |
| sort | 65.8 | 32.1 | 16.5 | 2.1× | 4.0× |
| string | 27.8 | 11.7 | 11.3 | 2.4× | 2.5× |
| closures | 2.1 | 17.2 | 2.3 | 0.1× | 0.9× |
| classes | 1.4 | 23.3 | 2.8 | 0.1× | 0.5× |
| collections | 52.8 | 27.9 | 21.3 | 1.9× | 2.5× |
| json | 24.9 | 11.6 | 11.5 | 2.2× | 2.2× |
| regexp | 12.4 | 3.0 | 1.8 | 4.2× | 7.0× |
| **Geometric mean** | | | | **0.6×** | **1.7×** |

## What the JIT does for them

- **fib**: a native function per JS function, called directly (arguments in registers, results unboxed through a second
  return register), the recursive call inlined two levels deep; the global `fib` is read through an epoch cache
- **closures**: the closure is inlined into the loop (checked by function identity), its context slots live in registers,
  `% 100000` is a multiplication
- **classes / objects**: constructors and methods inlined, objects that don't escape are never allocated (fields as values,
  also across loop iterations: the loop entered on-stack is peeled once), shape checks and loads moved out of the loop
- **loop**: `(s + i * i) % 1000003` with `i * i` past int32 runs as an int64 remainder of doubles
- array, sort, string, collections, json and regexp spend their time in builtins (and the GC); the JIT doesn't help them yet
