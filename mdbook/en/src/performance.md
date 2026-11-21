# Performance

## Against V8

`samples/benchv8` (j8 with its JIT, `--runs 10`, pinned to performance cores, load below 1). All 11 benchmarks produce
matching checksums on the three engines.

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

(A ratio below 1 means j8 is faster. From `samples/benchv8/benchmark.md`; the repository has the current numbers.)

## What the JIT does for them

- **fib**: a native function per JS function, called directly (arguments in registers, results unboxed through a second
  return register), the recursive call inlined two levels deep; the global `fib` read through an epoch cache;
- **closures**: the closure inlined into the loop (checked by function identity), its context slots in registers,
  `% 100000` as a multiplication;
- **classes / objects**: constructors and methods inlined, objects that don't escape never allocated (fields as values,
  across loop iterations too: the loop entered on-stack is peeled once), shape checks and loads hoisted out of the loop;
- **loop**: `(s + i * i) % 1000003` with `i * i` past int32 runs as an int64 remainder of doubles.

array, sort, string, collections, json and regexp spend their time in built-ins (and the GC); the JIT doesn't help them
yet, and they are next.

## Measuring

- timing covers compile + execute, a fresh environment per run, the shortest kept;
- pin to performance cores (`taskset -c 0-3`) and run with low load;
- judge a change by all the benchmarks, not just the one it targets;
- callgrind for line-level hot spots; `perf stat -e instructions:u` for instruction counts.
