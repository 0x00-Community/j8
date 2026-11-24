use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use wast::lexer::Lexer;
use wast::parser::{self, ParseBuffer};
use wast::{Wast, WastDirective};

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

struct Spec;

impl Spec {
    #[expect(clippy::unused_self, reason = "the executor hook arrives with the wasm interpreter")]
    fn run(&mut self, _d: &mut WastDirective<'_>) -> Outcome {
        Outcome::Unsupported
    }

    fn file(&mut self, path: &Path, rel: &str) -> Result<Vec<Case>, String> {
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
                let outcome = self.run(&mut d);
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
    let filters: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect();
    let files = Spec::files(&filters);
    if files.is_empty() {
        eprintln!("wasmspec: no .wast files under {ROOT} (git submodule update --init tests/wasm-testsuite)");
        return ExitCode::FAILURE;
    }
    let verbose = std::env::var_os(VERBOSE_ENV).is_some();
    let mut spec = Spec;
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
        if verbose && c.outcome == Outcome::Fail {
            println!("FAIL {}", c.id);
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
