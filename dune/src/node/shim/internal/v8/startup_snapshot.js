'use strict';
// internal/v8/startup_snapshot: dune never builds a startup snapshot
const isBuildingSnapshot = () => false;
module.exports = {
  runDeserializeCallbacks() {},
  throwIfBuildingSnapshot() {},
  addAfterUserSerializeCallback() {},
  namespace: {
    addSerializeCallback() {},
    addDeserializeCallback() {},
    setDeserializeMainFunction() {},
    isBuildingSnapshot,
  },
};
