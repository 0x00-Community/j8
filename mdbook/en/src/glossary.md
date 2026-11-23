# Glossary

| Term | Meaning |
|---|---|
| **acc (accumulator)** | the implicit result register of the bytecode; most instructions read or write it |
| **Agent** | the context of one run; JS operation semantics hang off it |
| **Context** | an execution context: the global object and built-in prototypes |
| **context slot / Env** | the heap environment holding variables that closures capture |
| **CodeCache** | bytecode shared by several executors |
| **deopt** | a failed speculation: machine code goes back to the interpreter |
| **EventLoop / EventLoopSet** | one JS event loop / the loop threads running many of them |
| **feedback slot** | where bytecode records types and shapes it saw; the JIT speculates on them |
| **FrameState** | what each interpreter register holds at a guard; frames are rebuilt from it on a deopt |
| **'scope brand** | the lifetime of one `Isolate::enter`; values carrying it can't leave that entry |
| **GcCell / ValueCell** | j8's mutable cells on paragc: a snapshot of a block of data / a single value |
| **ScopeGlobal** | a root (paragc's), surviving `enter` calls; `B` names its type (`ValueBrand`, `ContextBrand`) |
| **pending exception** | the slot a thrown value waits in, as in V8; `Error::Exception` only says something is there |
| **IC (inline cache)** | per property-access site, results cached by shape |
| **Ignition** | V8's interpreter; j8's bytecode matches it byte for byte |
| **Intrinsics** | the interface that installs built-ins into a `Context`, implemented by j8builtins |
| **Isolate** | an independent JS world: one GC heap |
| **NaN-boxing** | one 64-bit word per value, non-numbers packed into NaN bit patterns |
| **OSR (on-stack replacement)** | entering machine code in the middle of a loop |
| **paragc** | the GC library j8 uses |
| **Platform** | the thread pool, clocks, output and module loading the host lends the engine |
| **prelude** | JS a runtime runs after installing its bindings, to build its public API |
| **Realm** | the spec's realm: a set of global object and built-ins; j8 calls it `Context`, as V8 does |
| **rope** | a lazy string concatenation: two halves plus the total length |
| **Shape (hidden class)** | the layout of an object's properties, shared by objects built the same way |
| **short Star** | `Star0` … `Star15`: one-byte register stores without operands |
| **Tier** | the executor's JIT hook |
| **TDZ (temporal dead zone)** | the region before a `let` / `const` / `class` is initialized |
| **trampoline** | one jump per function, to its machine code or the interpreter |
| **`__j8_native`** | a runtime's hidden registry object |
