'use strict';
// internalBinding('errors') (src/node_errors.cc)
module.exports = {
  exitCodes: {
    kNoFailure: 0,
    kGenericUserError: 1,
    kInternalJSParseError: 3,
    kInternalJSEvaluationFailure: 4,
    kV8FatalError: 5,
    kInvalidFatalExceptionMonkeyPatching: 6,
    kExceptionInFatalExceptionHandler: 7,
    kInvalidCommandLineArgument: 9,
    kBootstrapFailure: 10,
    kInvalidCommandLineArgument2: 12,
    kUnsettledTopLevelAwait: 13,
    kStartupSnapshotFailure: 14,
    kAbort: 134,
  },
  noSideEffectsToString: (value) => {
    try { return String(value); } catch { return Object.prototype.toString.call(value); }
  },
  setPrepareStackTraceCallback() {},
  setGetSourceMapErrorSource() {},
  setSourceMapsEnabled() {},
  setMaybeCacheGeneratedSourceMap() {},
  setEnhanceStackForFatalException() {},
  triggerUncaughtException(error) { throw error; },
  getErrorSourcePositions: () => undefined,
};
