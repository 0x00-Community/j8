use std::io::{Read, Write};
use std::path::PathBuf;

use clap::Args;
use j8bytecode::Module;
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

/// `j8dbg compile`: JS ⇒ bytecode, written in the binary form.
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
        self.io.write(&Vec::from(&module))
    }
}

/// `j8dbg check`: read the binary form back (magic, instruction-set fingerprint, every field) and write it out again
/// re-encoded.
#[derive(Args)]
pub struct Check {
    #[command(flatten)]
    io: Io,
}

impl Check {
    pub fn run(&self) -> Result<(), String> {
        let (bytes, file) = self.io.bytes()?;
        let module = Module::try_from(bytes.as_slice()).map_err(|e| format!("{file}: {e}"))?;
        self.io.write(&Vec::from(&module))
    }
}
