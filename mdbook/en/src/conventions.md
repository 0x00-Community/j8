# Code conventions

j8 has explicit code rules; pull requests follow them.

## Structure

- **Private submodules** (`mod`), flattened by `pub use` at the crate root, ⛔ `pub mod`; visibility comes from private
  modules, and items are plain `pub`.
- **Dependency direction**: every layer depends only on `j8framework`; `j8framework` depends on no layer; `j8bytecode`
  is the root.
- `dune/` and `samples/*` are packages of their own (with an empty `[workspace]`) depending on the published `j8` **by
  version**, ⛔ path dependencies. When a new API isn't published yet, patch locally with `--config` on the command line
  (`[patch.crates-io]`), ⛔ committing the patch or a lock file generated with it.

## Rust

- ⛔ `static` and the `'static` lifetime (except `&'static str` names in enums); no global or static storage. Background
  tasks borrow the caller's data through `Task<'t>`.
- Strings are turned into enums **right after deserializing**, ⛔ deciding on strings.
- Conversions use `From` / `TryFrom` / `Into`, ⛔ hand-written `to_xxx` conversions (unless extra arguments are needed).
- Functions are methods of a type wherever possible: of the one mutable type among the parameters, or of the big struct.
- Prefer references, borrows and slices.
- Coefficients, thresholds and field encodings are named `const`s, ⛔ bare numbers.
- No provenance in code (measurement dates, machines, commit ids): that goes in commit messages.
- Reduce code with macros and generators: the bytecode tables come from V8's headers, heap structs use `#[heap]`,
  bindings use `#[j8::class]`.
- Source code carries no comments; documentation lives in the READMEs, the guides and this book.

## The heap

- Collection moves objects: ⛔ storing a `ScopeLocal` or an object address where it isn't traced.
- Write a value's slots only once it is on the heap.
- Stop-the-world collection only, ⛔ `mark_concurrently`.
- Lock-free structures are hand-written in j8lockfree, ⛔ crossbeam.

See [Heap and GC](heap.md#rules).

## Tests

- Unit tests pin only **certain facts backed by real data**; nothing uncertain (otherwise they pin wrong behavior).
- One unit test at a time, under `timeout 15`.
- Zero test262 regressions.

## Documentation

- English first: the repository and every published sub-package have a `README.md` (English, the default) and a
  `README.zh-CN.md` translation, linked to each other.
- This book is English with a Chinese translation; change both together.

## Commits

- j8 lives on GitHub ([0x00-Community/j8](https://github.com/0x00-Community/j8)): branches and pull requests, main is
  protected, and merged branches are deleted automatically.
