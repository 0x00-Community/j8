'use strict';
// internalBinding('fs_event_wrap') (src/fs_event_wrap.cc): dune does not watch files yet; start() fails with ENOSYS
const ERRNO_ENOSYS = -38;
class FSEvent {
  constructor() { this.onchange = undefined; this.initialized = false; }
  start() { return ERRNO_ENOSYS; }
  close() {}
  ref() {}
  unref() {}
  getAsyncId() { return -1; }
}
module.exports = { FSEvent };
