use std::io::{Read, Write};
use std::path::PathBuf;

use clap::Args;
use j8framework::Source;
use j8virtasm::Module;

/// Input (file / stdin / command line) and output (file / stdout).
#[derive(Args)]
pub struct Io {
    /// Input file; `-` reads stdin
    #[arg(required_unless_present = "eval", conflicts_with = "eval")]
    file: Option<PathBuf>,
    /// Input text given directly
    #[arg(short, long)]
    eval: Option<String>,
    /// Write to a file; defaults to stdout
    #[arg(short, long)]
    output: Option<PathBuf>,
}

impl Io {
    /// Input text and the file name used in errors
    fn input(&self) -> Result<(String, String), String> {
        match (&self.eval, &self.file) {
            (Some(code), _) => Ok((code.clone(), "<eval>".into())),
            (None, Some(p)) if p.as_os_str() == "-" => {
                let mut text = String::new();
                std::io::stdin().read_to_string(&mut text).map_err(|e| format!("<stdin>: {e}"))?;
                Ok((text, "<stdin>".into()))
            }
            (None, Some(p)) => {
                let text = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
                Ok((text, p.display().to_string()))
            }
            (None, None) => unreachable!("clap requires file or --eval"),
        }
    }

    fn write(&self, listing: &str) -> Result<(), String> {
        match &self.output {
            Some(p) => std::fs::write(p, listing).map_err(|e| format!("{}: {e}", p.display())),
            None => std::io::stdout().lock().write_all(listing.as_bytes()).map_err(|e| e.to_string()),
        }
    }
}

/// `j8dbg disasm`: compile JS to bytecode and print the disassembly listing.
#[derive(Args)]
pub struct Disasm {
    #[command(flatten)]
    io: Io,
}

impl Disasm {
    pub fn run(&self) -> Result<(), String> {
        let (text, file) = self.io.input()?;
        let module = Module::try_from(&Source { text: &text, file: &file }).map_err(|e| e.to_string())?;
        self.io.write(&module.to_string())
    }
}

/// `j8dbg asm`: assemble a disassembly listing (the output of `j8dbg disasm`) and print the normalized listing.
#[derive(Args)]
pub struct Asm {
    #[command(flatten)]
    io: Io,
}

impl Asm {
    pub fn run(&self) -> Result<(), String> {
        let (text, file) = self.io.input()?;
        let module = text.parse::<Module>().map_err(|e| format!("{file}: {e}"))?;
        self.io.write(&module.to_string())
    }
}
