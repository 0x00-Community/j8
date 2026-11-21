'use strict';
// internal/process/permission: dune has no permission model, everything is allowed
module.exports = {
  isEnabled: () => false,
  has: () => true,
  availableFlags: () => [],
};
