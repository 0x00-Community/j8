'use strict';
// internalBinding('trace_events') (src/node_trace_events.cc): dune has no tracing, every category is off
module.exports = {
  trace() {},
  isTraceCategoryEnabled: () => false,
  getCategoryEnabledBuffer: () => new Uint8Array(1),
  setTraceCategoryStateUpdateHandler() {},
  getEnabledCategories: () => undefined,
  availableCategories: [],
};
