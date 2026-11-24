use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use wast::lexer::Lexer;
use wast::parser::{self, ParseBuffer};
use j8bytecode::{WasmFuncType, WasmGlobalType, WasmHeapType, WasmLimits, WasmMemoryType, WasmModule, WasmRefType, WasmTableType, WasmValType};
use j8compiler::{CompileWasm, WasmError, WasmErrorKind};
use j8executor::{WasmExtern, WasmInstantiateError, WasmLinkError, WasmStore, WasmTrap, WasmValue};
use wast::core::{AbstractHeapType, HeapType, NanPattern, V128Pattern, WastArgCore, WastRetCore};
use wast::token::{Id, F32, F64};
use wast::{QuoteWat, Wast, WastArg, WastDirective, WastExecute, WastInvoke, WastRet};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/wasm-testsuite");

const BASELINE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/wasmspec.passing");

const UPDATE_ENV: &str = "J8_WASMSPEC_UPDATE";
const VERBOSE_ENV: &str = "J8_WASMSPEC_VERBOSE";

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

#[derive(Default)]
struct Spec {
    store: WasmStore,
    spectest: HashMap<&'static str, WasmExtern>,
    modules: HashMap<String, Rc<WasmModule>>,
    instances: HashMap<String, u32>,
    registered: HashMap<String, u32>,
    current: Option<u32>,
}

enum Run {
    Values(Vec<WasmValue>),
    Trap(WasmTrap),
    Broken,
}

impl Spec {
    fn new() -> Self {
        let mut s = Self::default();
        let i32t = WasmValType::I32;
        let print = |s: &mut WasmStore, params: &[WasmValType]| WasmExtern::Func(s.host_func(WasmFuncType { params: params.into(), results: Box::new([]) }, Box::new(|_| Ok(Vec::new()))));
        for (name, params) in [("print", &[][..]), ("print_i32", &[i32t][..]), ("print_i64", &[WasmValType::I64][..]), ("print_f32", &[WasmValType::F32][..]), ("print_f64", &[WasmValType::F64][..]), ("print_i32_f32", &[i32t, WasmValType::F32][..]), ("print_f64_f64", &[WasmValType::F64, WasmValType::F64][..])] {
            let f = print(&mut s.store, params);
            s.spectest.insert(name, f);
        }
        let global = |s: &mut WasmStore, content, v| WasmExtern::Global(s.host_global(WasmGlobalType { content, mutable: false }, v));
        let g = global(&mut s.store, i32t, WasmValue::I32(666));
        s.spectest.insert("global_i32", g);
        let g = global(&mut s.store, WasmValType::I64, WasmValue::I64(666));
        s.spectest.insert("global_i64", g);
        let g = global(&mut s.store, WasmValType::F32, WasmValue::F32(666.6_f32.to_bits()));
        s.spectest.insert("global_f32", g);
        let g = global(&mut s.store, WasmValType::F64, WasmValue::F64(666.6_f64.to_bits()));
        s.spectest.insert("global_f64", g);
        let funcref = WasmRefType { nullable: true, heap: WasmHeapType::Func };
        let t = s.store.host_table(WasmTableType { element: funcref, limits: WasmLimits { min: 10, max: Some(20) }, table64: false });
        s.spectest.insert("table", WasmExtern::Table(t));
        let t = s.store.host_table(WasmTableType { element: funcref, limits: WasmLimits { min: 10, max: Some(20) }, table64: true });
        s.spectest.insert("table64", WasmExtern::Table(t));
        let m = s.store.host_memory(WasmMemoryType { limits: WasmLimits { min: 1, max: Some(2) }, memory64: false, shared: false, page_size_log2: 16 });
        s.spectest.insert("memory", WasmExtern::Memory(m));
        s
    }

    fn import(&self, module: &str, name: &str) -> Option<WasmExtern> {
        if module == "spectest" {
            return self.spectest.get(name).copied();
        }
        self.store.export(*self.registered.get(module)?, name)
    }

    fn instantiate(&mut self, m: &Rc<WasmModule>) -> Result<u32, WasmInstantiateError> {
        let imports: Option<Vec<WasmExtern>> = m.imports.iter().map(|i| self.import(&i.module, &i.name)).collect();
        let imports = imports.ok_or(WasmInstantiateError::Link(WasmLinkError::MissingImport))?;
        self.store.instantiate(m, &imports)
    }

    fn compile(bytes: &[u8]) -> Result<Rc<WasmModule>, WasmError> {
        WasmModule::compile_wasm(bytes).map(Rc::new)
    }

    fn module(&mut self, wat: &mut QuoteWat<'_>, define: bool) -> Outcome {
        let name = wat.name().map(|i| i.name().to_owned());
        let Ok(bytes) = wat.encode() else { return Outcome::Fail };
        let Ok(m) = Self::compile(&bytes) else {
            self.current = None;
            return Outcome::Unsupported;
        };
        if define {
            if let Some(n) = name {
                self.modules.insert(n, m);
            }
            return Outcome::Pass;
        }
        match self.instantiate(&m) {
            Ok(i) => {
                self.current = Some(i);
                if let Some(n) = name {
                    self.instances.insert(n, i);
                }
                Outcome::Pass
            }
            Err(_) => {
                self.current = None;
                Outcome::Fail
            }
        }
    }

    fn target(&self, module: Option<Id<'_>>) -> Option<u32> {
        match module {
            Some(id) => self.instances.get(id.name()).copied(),
            None => self.current,
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
            WastArgCore::RefNull(HeapType::Abstract { ty: AbstractHeapType::Func | AbstractHeapType::NoFunc, .. }) => WasmValue::FuncRef(None),
            WastArgCore::RefNull(HeapType::Abstract { ty: AbstractHeapType::Extern | AbstractHeapType::NoExtern, .. }) => WasmValue::ExternRef(None),
            WastArgCore::RefNull(_) => WasmValue::Ref(None),
            WastArgCore::RefExtern(n) => WasmValue::ExternRef(Some(*n)),
            WastArgCore::RefHost(n) => WasmValue::Ref(Some(u64::from(*n))),
        })
    }

    fn invoke(&mut self, inv: &WastInvoke<'_>) -> Run {
        let Some(inst) = self.target(inv.module) else { return Run::Broken };
        let Some(WasmExtern::Func(f)) = self.store.export(inst, inv.name) else { return Run::Broken };
        let Some(args) = inv.args.iter().map(Self::arg).collect::<Option<Vec<_>>>() else { return Run::Broken };
        match self.store.invoke(f, &args) {
            Ok(v) => Run::Values(v),
            Err(WasmTrap::Unsupported) => Run::Broken,
            Err(t) => Run::Trap(t),
        }
    }

    fn execute(&mut self, exec: &mut WastExecute<'_>) -> Run {
        match exec {
            WastExecute::Invoke(inv) => self.invoke(inv),
            WastExecute::Get { module, global, .. } => {
                let Some(inst) = self.target(*module) else { return Run::Broken };
                match self.store.export(inst, global) {
                    Some(WasmExtern::Global(g)) => Run::Values(vec![self.store.global_value(g)]),
                    _ => Run::Broken,
                }
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
            (WastRetCore::RefNull(_), WasmValue::FuncRef(None) | WasmValue::ExternRef(None) | WasmValue::Ref(None)) | (WastRetCore::RefExtern(None), WasmValue::ExternRef(Some(_))) | (WastRetCore::RefFunc(_), WasmValue::FuncRef(Some(_))) => true,
            (WastRetCore::RefExtern(Some(a)), WasmValue::ExternRef(Some(b))) => *a == b,
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
                let Some(m) = module.and_then(|id| self.modules.get(id.name()).cloned()) else { return Outcome::Unsupported };
                match self.instantiate(&m) {
                    Ok(i) => {
                        self.current = Some(i);
                        if let Some(id) = instance {
                            self.instances.insert(id.name().to_owned(), i);
                        }
                        Outcome::Pass
                    }
                    Err(_) => Outcome::Fail,
                }
            }
            WastDirective::Register { name, module, .. } => match self.target(*module) {
                Some(i) => {
                    self.registered.insert((*name).to_owned(), i);
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
            WastDirective::AssertExhaustion { call, .. } => match self.invoke(call) {
                Run::Trap(WasmTrap::CallStackExhausted) => Outcome::Pass,
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
        *self = Self::new();
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
    let mut spec = Spec::new();
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
