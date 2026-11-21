'use strict';
// internal/bootstrap/realm: Node's bootstrap of the built-in module loader, reduced to the BuiltinModule queries the
// library makes; the modules themselves are dune's registry (`native.builtin`)
const native = globalThis.__j8_native;
const SCHEME_ONLY = ['sea', 'sqlite', 'test', 'test/reporters'];
const ids = () => native.builtinNames().filter((id) => !id.startsWith('internal/') && !id.startsWith('_'));

class BuiltinModule {
  static map = { has: (id) => ids().includes(id), get: (id) => (ids().includes(id) ? { id } : undefined) };
  static exists(id) { return ids().includes(id) || native.lib.source(id) !== undefined; }
  static canBeRequiredByUsers(id) { return ids().includes(id); }
  static canBeRequiredWithoutScheme(id) { return ids().includes(id) && !SCHEME_ONLY.includes(id); }
  static normalizeRequirableId(id) {
    if (id.startsWith('node:')) {
      const name = id.slice(5);
      if (BuiltinModule.canBeRequiredByUsers(name)) return name;
    } else if (BuiltinModule.canBeRequiredWithoutScheme(id)) {
      return id;
    }
    return undefined;
  }
  static isBuiltin(id) { return BuiltinModule.canBeRequiredWithoutScheme(id) || (id.startsWith('node:') && BuiltinModule.canBeRequiredByUsers(id.slice(5))); }
  static getSchemeOnlyModuleNames() { return SCHEME_ONLY.filter((id) => ids().includes(id)); }
  static getAllBuiltinModuleIds() { return ids(); }
  static allowRequireByUsers() {}
  static setRealmAllowRequireByUsers() {}
  static exposeInternals() {}
}

module.exports = { BuiltinModule, internalBinding: (name) => native.requireLib('internal/binding/' + name) };
