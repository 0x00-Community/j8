use core::fmt::Write as _;
use std::io::{Read, Write};
use std::path::PathBuf;

use clap::Args;
use j8bytecode::{Decoder, Module, OperandScale, OperandType, Register, SharedFunction};
use j8compiler::Compile;
use j8framework::{CodeKind, Source};

/// Input (file / stdin / command line) and output (file / stdout).
#[derive(Args)]
pub struct Io {
    /// Input file; `-` reads stdin
    #[arg(required_unless_present = "eval", conflicts_with = "eval")]
    file: Option<PathBuf>,
    /// Input given directly
    #[arg(short, long)]
    eval: Option<String>,
    /// Write to a file; defaults to stdout
    #[arg(short, long)]
    output: Option<PathBuf>,
}

impl Io {
    /// Input bytes and the file name used in errors
    fn bytes(&self) -> Result<(Vec<u8>, String), String> {
        match (&self.eval, &self.file) {
            (Some(code), _) => Ok((code.clone().into_bytes(), "<eval>".into())),
            (None, Some(p)) if p.as_os_str() == "-" => {
                let mut bytes = Vec::new();
                std::io::stdin().read_to_end(&mut bytes).map_err(|e| format!("<stdin>: {e}"))?;
                Ok((bytes, "<stdin>".into()))
            }
            (None, Some(p)) => {
                let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
                Ok((bytes, p.display().to_string()))
            }
            (None, None) => unreachable!("clap requires file or --eval"),
        }
    }

    fn write(&self, out: &[u8]) -> Result<(), String> {
        match &self.output {
            Some(p) => std::fs::write(p, out).map_err(|e| format!("{}: {e}", p.display())),
            None => std::io::stdout().lock().write_all(out).map_err(|e| e.to_string()),
        }
    }
}

/// `j8dbg compile`: JS ⇒ bytecode, listed per function the way V8's `--print-bytecode` lists instructions (to compare
/// with V8 line by line).
#[derive(Args)]
pub struct Compiled {
    #[command(flatten)]
    io: Io,
}

impl Compiled {
    pub fn run(&self) -> Result<(), String> {
        let (bytes, file) = self.io.bytes()?;
        let text = String::from_utf8(bytes).map_err(|e| format!("{file}: {e}"))?;
        let module = Module::compile(&Source { text: &text, file: &file }, CodeKind::Script).map_err(|e| e.to_string())?;
        let mut out = String::new();
        for (i, f) in module.functions.iter().enumerate() {
            Listing(f).write(&mut out, i).map_err(|e| e.to_string())?;
        }
        self.io.write(out.as_bytes())
    }
}

/// One function's listing.
struct Listing<'a>(&'a SharedFunction);

impl Listing<'_> {
    fn write(&self, out: &mut String, index: usize) -> core::fmt::Result {
        let f = self.0;
        let name = if f.name.is_empty() { format!("#{index}") } else { f.name.to_string() };
        writeln!(out, "== {name} {:?} params {} regs {}", f.kind, f.bytecode.parameter_count, f.bytecode.register_count)?;
        for d in Decoder::from(&f.bytecode.code[..]) {
            let d = d.map_err(|_| core::fmt::Error)?;
            let ops: Vec<String> = d.bytecode.operand_types().iter().zip(&d.operands).map(|(t, &v)| Self::operand(*t, v)).collect();
            let wide = match d.scale {
                OperandScale::Single => "",
                OperandScale::Double => ".Wide",
                OperandScale::Quadruple => ".ExtraWide",
            };
            writeln!(out, "  @{:4} {:?}{wide} {}", d.offset, d.bytecode, ops.join(", "))?;
        }
        Ok(())
    }

    fn operand(t: OperandType, v: i64) -> String {
        match t {
            _ if t.is_register() => {
                let r = Register::from(i32::try_from(v).unwrap_or(0));
                if r == Register::receiver() {
                    "<this>".into()
                } else if r == Register::current_context() {
                    "<context>".into()
                } else if r == Register::function_closure() {
                    "<closure>".into()
                } else {
                    r.parameter_index().map_or_else(|| format!("r{}", r.0), |i| format!("a{}", i - 1))
                }
            }
            OperandType::FeedbackSlot => format!("FBV[{v}]"),
            OperandType::ConstantPoolIndex | OperandType::Imm | OperandType::UImm => format!("[{v}]"),
            _ => format!("#{v}"),
        }
    }
}
