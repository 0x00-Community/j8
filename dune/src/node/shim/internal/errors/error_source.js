'use strict';
// internal/errors/error_source: j8 has no stack traces yet, so the source of an error is unknown
module.exports = {
  getErrorSourceLocation: () => undefined,
  getErrorSourceExpression: () => undefined,
};
