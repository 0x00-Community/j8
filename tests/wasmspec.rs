use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use wast::lexer::Lexer;
use wast::parser::{self, ParseBuffer};
use j8bytecode::{WasmFuncType, WasmGlobalType, WasmHeapType, WasmLimits, WasmMemoryType, WasmModule, WasmRefType, WasmTableType, WasmValType};
use j8compiler::{CompileWasm, WasmError, WasmErrorKind};
use j8executor::{FuncBrand, GlobalBrand, InstanceBrand, MemoryBrand, TableBrand, TagBrand, TypesBrand, WasmCompiled, WasmExtern, WasmFunc, WasmHost, WasmInstantiateError, WasmLinkError, WasmRef, WasmStore, WasmThrow, WasmTrap, WasmTypes, WasmValue};
use j8framework::{CreateParams, Isolate, Mutation, ScopeGlobal};
use j8platform::DefaultPlatform;
use wast::core::{NanPattern, V128Pattern, WastArgCore, WastRetCore};
use wast::token::{Id, F32, F64};
use wast::{QuoteWat, Wast, WastArg, WastDirective, WastExecute, WastInvoke, WastRet};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/wasm-testsuite");

const BASELINE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/wasmspec.passing");

const UPDATE_ENV: &str = "J8_WASMSPEC_UPDATE";
const VERBOSE_ENV: &str = "J8_WASMSPEC_VERBOSE";
const COLLECT_ENV: &str = "J8_WASMSPEC_COLLECT";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Module,
    Register,
    Invoke,
    AssertReturn,
    AssertTrap,
    AssertExhaustion,
    AssertException,
    AssertMalformed,
    AssertInvalid,
    AssertUnlinkable,
    Other,
}

impl From<&WastDirective<'_>> for Kind {
    fn from(d: &WastDirective<'_>) -> Self {
        match d {
            WastDirective::Module(_) | WastDirective::ModuleDefinition(_) | WastDirective::ModuleInstance { .. } => Self::Module,
            WastDirective::Register { .. } => Self::Register,
            WastDirective::Invoke(_) => Self::Invoke,
            WastDirective::AssertReturn { .. } => Self::AssertReturn,
            WastDirective::AssertTrap { .. } => Self::AssertTrap,
            WastDirective::AssertExhaustion { .. } => Self::AssertExhaustion,
            WastDirective::AssertException { .. } => Self::AssertException,
            WastDirective::AssertMalformed { .. } => Self::AssertMalformed,
            WastDirective::AssertInvalid { .. } => Self::AssertInvalid,
            WastDirective::AssertUnlinkable { .. } => Self::AssertUnlinkable,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Pass,
    Fail,
    Unsupported,
}

struct Case {
    id: String,
    kind: Kind,
    outcome: Outcome,
}

enum Stashed<'p> {
    Func(ScopeGlobal<'p, FuncBrand>),
    Table(ScopeGlobal<'p, TableBrand>),
    Memory(ScopeGlobal<'p, MemoryBrand>),
    Global(ScopeGlobal<'p, GlobalBrand>),
    Tag(ScopeGlobal<'p, TagBrand>),
}

impl Stashed<'_> {
    fn stash<'s>(mc: &Mutation<'s>, e: WasmExtern<'s>) -> Self {
        match e {
            WasmExtern::Func(x) => Self::Func(mc.stash(x)),
            WasmExtern::Table(x) => Self::Table(mc.stash(x)),
            WasmExtern::Memory(x) => Self::Memory(mc.stash(x)),
            WasmExtern::Global(x) => Self::Global(mc.stash(x)),
            WasmExtern::Tag(x) => Self::Tag(mc.stash(x)),
        }
    }

    fn fetch<'s>(&self, mc: &Mutation<'s>) -> WasmExtern<'s> {
        match self {
            Self::Func(x) => WasmExtern::Func(mc.fetch(x)),
            Self::Table(x) => WasmExtern::Table(mc.fetch(x)),
            Self::Memory(x) => WasmExtern::Memory(mc.fetch(x)),
            Self::Global(x) => WasmExtern::Global(mc.fetch(x)),
            Self::Tag(x) => WasmExtern::Tag(mc.fetch(x)),
        }
    }
}

struct SpecHost;

impl<'s> WasmHost<'s> for SpecHost {
    fn call(&mut self, _: &Mutation<'s>, _: &'s WasmFunc<'s>, _: &WasmFuncType, _: &[(u64, u128)]) -> Result<Vec<(u64, u128)>, WasmThrow> {
        Ok(Vec::new())
    }
}

struct Spec<'p> {
    isolate: Isolate<'p>,
    st: State<'p>,
}

struct State<'p> {
    types: Option<ScopeGlobal<'p, TypesBrand>>,
    spectest: HashMap<&'static str, Stashed<'p>>,
    modules: HashMap<String, Arc<WasmCompiled>>,
    instances: Vec<ScopeGlobal<'p, InstanceBrand>>,
    named: HashMap<String, usize>,
    registered: HashMap<String, usize>,
    current: Option<usize>,
    collect: bool,
}

impl State<'_> {
    fn import<'s>(&self, mc: &Mutation<'s>, module: &str, name: &str) -> Option<WasmExtern<'s>> {
        if module == "spectest" {
            return self.spectest.get(name).map(|e| e.fetch(mc));
        }
        let inst = mc.fetch(&self.instances[*self.registered.get(module)?]);
        WasmStore::export(inst.get(), name)
    }
}

enum Run {
    Values(Vec<WasmValue>),
    Trap(WasmThrow),
    Broken,
}

impl<'p> Spec<'p> {
    fn new(platform: &'p DefaultPlatform) -> Self {
        let mut s = Self {
            isolate: Isolate::new(platform, CreateParams::default()),
            st: State {
            types: None,
            spectest: HashMap::new(),
            modules: HashMap::new(),
            instances: Vec::new(),
            named: HashMap::new(),
            registered: HashMap::new(),
            current: None,
            collect: std::env::var_os(COLLECT_ENV).is_some(),
            },
        };
        s.reset();
        s
    }

    fn reset(&mut self) {
        self.st.spectest.clear();
        self.st.modules.clear();
        self.st.instances.clear();
        self.st.named.clear();
        self.st.registered.clear();
        self.st.current = None;
        let i32t = WasmValType::I32;
        let prints = [("print", &[][..]), ("print_i32", &[i32t][..]), ("print_i64", &[WasmValType::I64][..]), ("print_f32", &[WasmValType::F32][..]), ("print_f64", &[WasmValType::F64][..]), ("print_i32_f32", &[i32t, WasmValType::F32][..]), ("print_f64_f64", &[WasmValType::F64, WasmValType::F64][..])];
        let globals = [("global_i32", i32t, WasmValue::I32(666)), ("global_i64", WasmValType::I64, WasmValue::I64(666)), ("global_f32", WasmValType::F32, WasmValue::F32(666.6_f32.to_bits())), ("global_f64", WasmValType::F64, WasmValue::F64(666.6_f64.to_bits()))];
        let funcref = WasmRefType { nullable: true, heap: WasmHeapType::Func };
        let spectest = &mut self.st.spectest;
        self.st.types = Some(self.isolate.scope(|mc| {
            let types = WasmTypes::alloc(mc);
            let mut host = SpecHost;
            let store = WasmStore::new(mc, types, &mut host);
            for (name, params) in prints {
                let f = store.host_func(&WasmFuncType { params: params.into(), results: Box::new([]) }, 0);
                spectest.insert(name, Stashed::stash(mc, WasmExtern::Func(f)));
            }
            for (name, content, v) in globals {
                let g = store.host_global(WasmGlobalType { content, mutable: false }, v.words());
                spectest.insert(name, Stashed::stash(mc, WasmExtern::Global(g)));
            }
            for (name, table64) in [("table", false), ("table64", true)] {
                let t = store.host_table(WasmTableType { element: funcref, limits: WasmLimits { min: 10, max: Some(20) }, table64 }, 0);
                spectest.insert(name, Stashed::stash(mc, WasmExtern::Table(t)));
            }
            let m = store.host_memory(WasmMemoryType { limits: WasmLimits { min: 1, max: Some(2) }, memory64: false, shared: false, page_size_log2: 16 });
            spectest.insert("memory", Stashed::stash(mc, WasmExtern::Memory(m)));
            mc.stash(types)
        }));
    }

    fn with<R>(&mut self, f: impl for<'s> FnOnce(&Mutation<'s>, &mut WasmStore<'_, 's>, &State<'p>) -> R) -> R {
        let this = &self.st;
        self.isolate.scope(|mc| {
            let Some(types) = &this.types else { unreachable!("reset stashes the type registry") };
            let mut host = SpecHost;
            let mut store = WasmStore::new(mc, mc.fetch(types), &mut host);
            f(mc, &mut store, this)
        })
    }

    fn instantiate(&mut self, m: &Arc<WasmCompiled>) -> Result<usize, WasmInstantiateError> {
        let r = self.with(|mc, store, this| {
            let imports: Option<Vec<WasmExtern<'_>>> = m.module.imports.iter().map(|i| this.import(mc, &i.module, &i.name)).collect();
            let imports = imports.ok_or(WasmInstantiateError::Link(WasmLinkError::MissingImport))?;
            store.instantiate(m, &imports).map(|i| mc.stash(i))
        })?;
        self.st.instances.push(r);
        Ok(self.st.instances.len() - 1)
    }

    fn compile(bytes: &[u8]) -> Result<Arc<WasmCompiled>, WasmError> {
        WasmModule::compile_wasm(bytes).map(|m| Arc::new(WasmCompiled::from(m)))
    }

    fn module(&mut self, wat: &mut QuoteWat<'_>, define: bool) -> Outcome {
        let name = wat.name().map(|i| i.name().to_owned());
        let Ok(bytes) = wat.encode() else { return Outcome::Fail };
        let Ok(m) = Self::compile(&bytes) else {
            self.st.current = None;
            return Outcome::Unsupported;
        };
        if define {
            if let Some(n) = name {
                self.st.modules.insert(n, m);
            }
            return Outcome::Pass;
        }
        match self.instantiate(&m) {
            Ok(i) => {
                self.st.current = Some(i);
                if let Some(n) = name {
                    self.st.named.insert(n, i);
                }
                Outcome::Pass
            }
            Err(_) => {
                self.st.current = None;
                Outcome::Fail
            }
        }
    }

    fn target(&self, module: Option<Id<'_>>) -> Option<usize> {
        match module {
            Some(id) => self.st.named.get(id.name()).copied(),
            None => self.st.current,
        }
    }

    fn arg(a: &WastArg<'_>) -> Option<WasmValue> {
        let WastArg::Core(c) = a else { return None };
        Some(match c {
            WastArgCore::I32(v) => WasmValue::I32(*v),
            WastArgCore::I64(v) => WasmValue::I64(*v),
            WastArgCore::F32(v) => WasmValue::F32(v.bits),
            WastArgCore::F64(v) => WasmValue::F64(v.bits),
            WastArgCore::V128(v) => WasmValue::V128(u128::from_le_bytes(v.to_le_bytes())),
            WastArgCore::RefNull(_) => WasmValue::Ref(WasmRef::Null),
            WastArgCore::RefExtern(n) | WastArgCore::RefHost(n) => WasmValue::Ref(WasmRef::Host(*n)),
        })
    }

    fn invoke(&mut self, inv: &WastInvoke<'_>) -> Run {
        let Some(inst) = self.target(inv.module) else { return Run::Broken };
        let Some(args) = inv.args.iter().map(Self::arg).collect::<Option<Vec<_>>>() else { return Run::Broken };
        let args: Vec<(u64, u128)> = args.iter().map(|a| a.words()).collect();
        self.with(|mc, store, this| {
            let Some(WasmExtern::Func(f)) = WasmStore::export(mc.fetch(&this.instances[inst]).get(), inv.name) else { return Run::Broken };
            let ty = store.func_type(f.get());
            match store.invoke(f, &args) {
                Ok(v) => Run::Values(ty.results.iter().zip(v).map(|(&t, w)| store.value(t, w)).collect()),
                Err(WasmThrow::Trap(WasmTrap::Unsupported)) => Run::Broken,
                Err(t) => Run::Trap(t),
            }
        })
    }

    fn execute(&mut self, exec: &mut WastExecute<'_>) -> Run {
        match exec {
            WastExecute::Invoke(inv) => self.invoke(inv),
            WastExecute::Get { module, global, .. } => {
                let Some(inst) = self.target(*module) else { return Run::Broken };
                self.with(|mc, store, this| match WasmStore::export(mc.fetch(&this.instances[inst]).get(), global) {
                    Some(WasmExtern::Global(g)) => Run::Values(vec![store.value(g.get().ty.content, g.get().get())]),
                    _ => Run::Broken,
                })
            }
            WastExecute::Wat(w) => {
                let Ok(bytes) = w.encode() else { return Run::Broken };
                let Ok(m) = Self::compile(&bytes) else { return Run::Broken };
                match self.instantiate(&m) {
                    Ok(_) => Run::Values(Vec::new()),
                    Err(WasmInstantiateError::Trap(t)) => Run::Trap(t),
                    Err(WasmInstantiateError::Link(_)) => Run::Broken,
                }
            }
        }
    }

    fn nan32(p: NanPattern<F32>, bits: u32) -> bool {
        match p {
            NanPattern::CanonicalNan => bits & 0x7fff_ffff == 0x7fc0_0000,
            NanPattern::ArithmeticNan => bits & 0x7fc0_0000 == 0x7fc0_0000,
            NanPattern::Value(v) => v.bits == bits,
        }
    }

    fn nan64(p: NanPattern<F64>, bits: u64) -> bool {
        match p {
            NanPattern::CanonicalNan => bits & 0x7fff_ffff_ffff_ffff == 0x7ff8_0000_0000_0000,
            NanPattern::ArithmeticNan => bits & 0x7ff8_0000_0000_0000 == 0x7ff8_0000_0000_0000,
            NanPattern::Value(v) => v.bits == bits,
        }
    }

    fn vector(p: &V128Pattern, v: u128) -> bool {
        let b = v.to_le_bytes();
        let chunk = |i: usize, n: usize| b[i * n..(i + 1) * n].iter().rev().fold(0u64, |a, &x| (a << 8) | u64::from(x));
        match p {
            V128Pattern::I8x16(l) => l.iter().enumerate().all(|(i, &x)| chunk(i, 1) == u64::from(x.cast_unsigned())),
            V128Pattern::I16x8(l) => l.iter().enumerate().all(|(i, &x)| chunk(i, 2) == u64::from(x.cast_unsigned())),
            V128Pattern::I32x4(l) => l.iter().enumerate().all(|(i, &x)| chunk(i, 4) == u64::from(x.cast_unsigned())),
            V128Pattern::I64x2(l) => l.iter().enumerate().all(|(i, &x)| chunk(i, 8) == x.cast_unsigned()),
            V128Pattern::F32x4(l) => l.iter().enumerate().all(|(i, &x)| Self::nan32(x, u32::try_from(chunk(i, 4)).unwrap_or(0))),
            V128Pattern::F64x2(l) => l.iter().enumerate().all(|(i, &x)| Self::nan64(x, chunk(i, 8))),
        }
    }

    fn matches(want: &WastRetCore<'_>, got: WasmValue) -> bool {
        match (want, got) {
            (WastRetCore::I32(a), WasmValue::I32(b)) => *a == b,
            (WastRetCore::I64(a), WasmValue::I64(b)) => *a == b,
            (WastRetCore::F32(p), WasmValue::F32(b)) => Self::nan32(*p, b),
            (WastRetCore::F64(p), WasmValue::F64(b)) => Self::nan64(*p, b),
            (WastRetCore::RefNull(_), WasmValue::Ref(WasmRef::Null))
            | (WastRetCore::RefFunc(_), WasmValue::Ref(WasmRef::Func))
            | (WastRetCore::RefI31 | WastRetCore::RefEq, WasmValue::Ref(WasmRef::I31(_)))
            | (WastRetCore::RefStruct | WastRetCore::RefEq, WasmValue::Ref(WasmRef::Struct))
            | (WastRetCore::RefArray | WastRetCore::RefEq, WasmValue::Ref(WasmRef::Array)) => true,
            (WastRetCore::RefExtern(None) | WastRetCore::RefAny, WasmValue::Ref(r)) => r != WasmRef::Null,
            (WastRetCore::RefExtern(Some(a)) | WastRetCore::RefHost(a), WasmValue::Ref(WasmRef::Host(b))) => *a == b,
            (WastRetCore::V128(p), WasmValue::V128(v)) => Self::vector(p, v),
            (WastRetCore::Either(alts), v) => alts.iter().any(|a| Self::matches(a, v)),
            _ => false,
        }
    }

    fn run(&mut self, d: &mut WastDirective<'_>) -> Outcome {
        match d {
            WastDirective::Module(m) => self.module(m, false),
            WastDirective::ModuleDefinition(m) => self.module(m, true),
            WastDirective::ModuleInstance { instance, module, .. } => {
                let Some(m) = module.and_then(|id| self.st.modules.get(id.name()).cloned()) else { return Outcome::Unsupported };
                match self.instantiate(&m) {
                    Ok(i) => {
                        self.st.current = Some(i);
                        if let Some(id) = instance {
                            self.st.named.insert(id.name().to_owned(), i);
                        }
                        Outcome::Pass
                    }
                    Err(_) => Outcome::Fail,
                }
            }
            WastDirective::Register { name, module, .. } => match self.target(*module) {
                Some(i) => {
                    self.st.registered.insert((*name).to_owned(), i);
                    Outcome::Pass
                }
                None => Outcome::Unsupported,
            },
            WastDirective::Invoke(inv) => match self.invoke(inv) {
                Run::Values(_) => Outcome::Pass,
                Run::Trap(_) => Outcome::Fail,
                Run::Broken => Outcome::Unsupported,
            },
            WastDirective::AssertReturn { exec, results, .. } => match self.execute(exec) {
                Run::Values(v) => {
                    let ok = v.len() == results.len() && results.iter().zip(&v).all(|(r, &g)| matches!(r, WastRet::Core(c) if Self::matches(c, g)));
                    if ok { Outcome::Pass } else { Outcome::Fail }
                }
                Run::Trap(_) => Outcome::Fail,
                Run::Broken => Outcome::Unsupported,
            },
            WastDirective::AssertTrap { exec, .. } => match self.execute(exec) {
                Run::Trap(_) => Outcome::Pass,
                Run::Values(_) => Outcome::Fail,
                Run::Broken => Outcome::Unsupported,
            },
            WastDirective::AssertException { exec, .. } => match self.execute(exec) {
                Run::Trap(WasmThrow::Exception(_)) => Outcome::Pass,
                Run::Broken => Outcome::Unsupported,
                _ => Outcome::Fail,
            },
            WastDirective::AssertExhaustion { call, .. } => match self.invoke(call) {
                Run::Trap(WasmThrow::Trap(WasmTrap::CallStackExhausted)) => Outcome::Pass,
                Run::Broken => Outcome::Unsupported,
                _ => Outcome::Fail,
            },
            WastDirective::AssertMalformed { module, .. } | WastDirective::AssertInvalid { module, .. } => {
                let Ok(bytes) = module.encode() else { return Outcome::Pass };
                match WasmModule::compile_wasm(&bytes) {
                    Err(e) if e.kind == WasmErrorKind::Invalid => Outcome::Pass,
                    Err(_) => Outcome::Unsupported,
                    Ok(_) => Outcome::Fail,
                }
            }
            WastDirective::AssertUnlinkable { module, .. } => {
                let Ok(bytes) = module.encode() else { return Outcome::Fail };
                let Ok(m) = Self::compile(&bytes) else { return Outcome::Unsupported };
                match self.instantiate(&m) {
                    Err(WasmInstantiateError::Link(_)) => Outcome::Pass,
                    _ => Outcome::Fail,
                }
            }
            _ => Outcome::Unsupported,
        }
    }

    fn file(&mut self, path: &Path, rel: &str) -> Result<Vec<Case>, String> {
        self.reset();
        let text = std::fs::read_to_string(path).map_err(|e| format!("{rel}: {e}"))?;
        let mut lexer = Lexer::new(&text);
        lexer.allow_confusing_unicode(true);
        let buf = ParseBuffer::new_with_lexer(lexer).map_err(|e| format!("{rel}: {e}"))?;
        let wast = parser::parse::<Wast>(&buf).map_err(|e| format!("{rel}: {e}"))?;
        Ok(wast
            .directives
            .into_iter()
            .map(|mut d| {
                let (line, _) = d.span().linecol_in(&text);
                let kind = Kind::from(&d);
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run(&mut d))).unwrap_or(Outcome::Fail);
                if self.st.collect {
                    self.isolate.collect();
                }
                Case { id: format!("{rel}:{}", line + 1), kind, outcome }
            })
            .collect())
    }

    fn files(filters: &[String]) -> Vec<(PathBuf, String)> {
        let mut out: Vec<(PathBuf, String)> = std::fs::read_dir(ROOT)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "wast"))
            .filter_map(|p| {
                let rel = p.file_name()?.to_string_lossy().into_owned();
                (filters.is_empty() || filters.iter().any(|f| rel.starts_with(f.as_str()))).then_some((p, rel))
            })
            .collect();
        out.sort_by(|a, b| a.1.cmp(&b.1));
        out
    }
}

fn main() -> ExitCode {
    std::panic::set_hook(Box::new(|_| {}));
    let filters: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect();
    let files = Spec::files(&filters);
    if files.is_empty() {
        eprintln!("wasmspec: no .wast files under {ROOT} (git submodule update --init tests/wasm-testsuite)");
        return ExitCode::FAILURE;
    }
    let verbose = std::env::var_os(VERBOSE_ENV).is_some();
    let platform = DefaultPlatform::default();
    let mut spec = Spec::new(&platform);
    let mut cases = Vec::new();
    let mut broken = 0;
    for (path, rel) in &files {
        match spec.file(path, rel) {
            Ok(c) => cases.extend(c),
            Err(e) => {
                broken += 1;
                eprintln!("wasmspec: {e}");
            }
        }
    }
    let mut kinds: BTreeMap<Kind, [usize; 3]> = BTreeMap::new();
    for c in &cases {
        let row = kinds.entry(c.kind).or_default();
        row[0] += 1;
        match c.outcome {
            Outcome::Pass => row[1] += 1,
            Outcome::Fail => row[2] += 1,
            Outcome::Unsupported => {}
        }
        if verbose {
            match c.outcome {
                Outcome::Fail => println!("FAIL {}", c.id),
                Outcome::Unsupported => println!("SKIP {}", c.id),
                Outcome::Pass => {}
            }
        }
    }
    println!("{:<20}{:>8}{:>8}{:>8}", "wasmspec", "total", "pass", "fail");
    for (k, [t, p, f]) in &kinds {
        println!("{:<20}{t:>8}{p:>8}{f:>8}", format!("{k:?}"));
    }
    let passing: BTreeSet<&str> = cases.iter().filter(|c| c.outcome == Outcome::Pass).map(|c| c.id.as_str()).collect();
    println!("{} files ({broken} unparsable), {} directives, {} passing", files.len(), cases.len(), passing.len());
    let base_text = std::fs::read_to_string(BASELINE).unwrap_or_default();
    let base: BTreeSet<&str> = base_text.lines().filter(|l| !l.is_empty()).collect();
    let ran: BTreeSet<&str> = cases.iter().map(|c| c.id.as_str()).collect();
    let lost: Vec<&&str> = base.iter().filter(|b| ran.contains(**b) && !passing.contains(**b)).collect();
    if std::env::var_os(UPDATE_ENV).is_some() {
        let merged: BTreeSet<&str> = base.iter().copied().filter(|b| !ran.contains(b)).chain(passing.iter().copied()).collect();
        let body: String = merged.iter().flat_map(|l| [*l, "\n"]).collect();
        if let Err(e) = std::fs::write(BASELINE, body) {
            eprintln!("wasmspec: {BASELINE}: {e}");
            return ExitCode::FAILURE;
        }
        println!("baseline: {} passing", merged.len());
    }
    for l in &lost {
        println!("REGRESSED {l}");
    }
    if lost.is_empty() && broken == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
