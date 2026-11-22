'use strict';
// internalBinding('performance') (src/node_perf.cc), on dune's clock: `now` is milliseconds since the process started
const { clock } = globalThis.__j8_native;

const constants = {
  NODE_PERFORMANCE_GC_MAJOR: 4,
  NODE_PERFORMANCE_GC_MINOR: 1,
  NODE_PERFORMANCE_GC_INCREMENTAL: 8,
  NODE_PERFORMANCE_GC_WEAKCB: 16,
  NODE_PERFORMANCE_GC_FLAGS_NO: 0,
  NODE_PERFORMANCE_GC_FLAGS_CONSTRUCT_RETAINED: 2,
  NODE_PERFORMANCE_GC_FLAGS_FORCED: 4,
  NODE_PERFORMANCE_GC_FLAGS_SYNCHRONOUS_PHANTOM_PROCESSING: 8,
  NODE_PERFORMANCE_GC_FLAGS_ALL_AVAILABLE_GARBAGE: 16,
  NODE_PERFORMANCE_GC_FLAGS_ALL_EXTERNAL_MEMORY: 32,
  NODE_PERFORMANCE_GC_FLAGS_SCHEDULE_IDLE: 64,
  NODE_PERFORMANCE_ENTRY_TYPE_GC: 0,
  NODE_PERFORMANCE_ENTRY_TYPE_HTTP: 1,
  NODE_PERFORMANCE_ENTRY_TYPE_HTTP2: 2,
  NODE_PERFORMANCE_ENTRY_TYPE_NET: 3,
  NODE_PERFORMANCE_ENTRY_TYPE_DNS: 4,
  NODE_PERFORMANCE_MILESTONE_TIME_ORIGIN_TIMESTAMP: 0,
  NODE_PERFORMANCE_MILESTONE_TIME_ORIGIN: 1,
  NODE_PERFORMANCE_MILESTONE_ENVIRONMENT: 2,
  NODE_PERFORMANCE_MILESTONE_NODE_START: 3,
  NODE_PERFORMANCE_MILESTONE_V8_START: 4,
  NODE_PERFORMANCE_MILESTONE_LOOP_START: 5,
  NODE_PERFORMANCE_MILESTONE_LOOP_EXIT: 6,
  NODE_PERFORMANCE_MILESTONE_BOOTSTRAP_COMPLETE: 7,
};
const NOT_YET = -1;
const startedAt = clock.wall() - clock.now();
const milestones = new Float64Array([startedAt * 1e3, 0, 0, 0, 0, 0, NOT_YET, 0]);

module.exports = {
  constants,
  milestones,
  observerCounts: new Uint32Array(5),
  now: () => clock.now(),
  loopIdleTime: () => 0,
  notify() {},
  setupObservers() {},
  markBootstrapComplete() {},
  installGarbageCollectionTracking() {},
  removeGarbageCollectionTracking() {},
  uvMetricsInfo: () => [0, 0, 0],
};
