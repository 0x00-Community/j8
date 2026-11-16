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

`--runs 5`, 16-core machine, load average about 6 while measuring (other processes running); the numbers are noisy, only the order of magnitude matters. All 11 benchmarks produce matching checksums on all three engines.

| Benchmark | j8 ms | V8 jitless ms | V8 ms | j8 / jitless | j8 / V8 |
|---|---:|---:|---:|---:|---:|
| fib | 41.6 | 9.5 | 2.1 | 4.4× | 19.5× |
| loop | 316.8 | 182.9 | 81.1 | 1.7× | 3.9× |
| objects | 76.1 | 15.2 | 2.6 | 5.0× | 28.9× |
| array | 74.2 | 14.5 | 7.2 | 5.1× | 10.3× |
| sort | 121.8 | 38.5 | 18.6 | 3.2× | 6.6× |
| string | 46.2 | 12.7 | 12.0 | 3.6× | 3.8× |
| closures | 77.5 | 19.3 | 2.6 | 4.0× | 29.5× |
| classes | 127.7 | 25.8 | 3.1 | 5.0× | 40.8× |
| collections | 69.4 | 31.0 | 25.4 | 2.2× | 2.7× |
| json | 37.7 | 12.9 | 13.0 | 2.9× | 2.9× |
| regexp | 19.3 | 3.1 | 2.0 | 6.1× | 9.7× |
| **Geometric mean** | | | | **3.7×** | **9.4×** |

## Analysis (callgrind per-line instruction counts)

- **Interpreter dispatch**: a simple bytecode costs about 14 machine instructions (fetch + jump table); `run` is one big `match`, and its
  register allocation is very sensitive to changes: adding a branch or changing a call's arguments inside it can push the state pointer onto
  the stack, adding one or two loads per dispatch, and fib / closures / loop regress 3–5 % with it. Measure all benchmarks when changing it
- **Calls**: each call + return costs about 400 instructions (function table lookup, opening a frame and filling registers, pushing and
  popping the frame); the JIT only takes numeric loops, so call-heavy benchmarks (fib, closures, classes) are 20–40× behind V8 with JIT
- **Allocation**: object property slots, strings longer than 23 bytes and rope nodes all go through glibc malloc (freed in batches on
  collection, about 250 instructions each); 15–25 % of objects / classes / string
- **loop**: only 1.7× behind jitless; what remains is mostly the 64-bit integer division of the remainder operation itself
- **regexp**: pure literal patterns already use substring search; patterns like `\d+` still go through the regress backtracking engine
