use std::io::{Read, Write};
use std::path::PathBuf;

use clap::Args;
use j8framework::Source;
use j8bytecode::Module;

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

    /// Input text and the file name used in errors
    fn input(&self) -> Result<(String, String), String> {
        let (bytes, file) = self.bytes()?;
        String::from_utf8(bytes).map(|t| (t, file.clone())).map_err(|e| format!("{file}: {e}"))
    }

    fn write(&self, out: &[u8]) -> Result<(), String> {
        match &self.output {
            Some(p) => std::fs::write(p, out).map_err(|e| format!("{}: {e}", p.display())),
            None => std::io::stdout().lock().write_all(out).map_err(|e| e.to_string()),
        }
    }

    /// The listing, or with `binary` the binary form
    fn module(&self, module: &Module, binary: bool) -> Result<(), String> {
        if binary { self.write(&Vec::from(module)) } else { self.write(module.to_string().as_bytes()) }
    }
}

/// `j8dbg disasm`: compile JS to bytecode and print the disassembly listing.
#[derive(Args)]
pub struct Disasm {
    #[command(flatten)]
    io: Io,
    /// Write the binary form instead of the listing
    #[arg(short, long)]
    binary: bool,
}

impl Disasm {
    pub fn run(&self) -> Result<(), String> {
        let (text, file) = self.io.input()?;
        let module = <Module as j8compiler::Compile>::compile(&Source { text: &text, file: &file }, j8framework::CodeKind::Script).map_err(|e| e.to_string())?;
        self.io.module(&module, self.binary)
    }
}

/// `j8dbg asm`: read bytecode, a listing (the output of `j8dbg disasm`) or the binary form (told apart by
/// `Module::MAGIC`), and print the normalized listing.
#[derive(Args)]
pub struct Asm {
    #[command(flatten)]
    io: Io,
    /// Write the binary form instead of the listing
    #[arg(short, long)]
    binary: bool,
}

impl Asm {
    pub fn run(&self) -> Result<(), String> {
        let (bytes, file) = self.io.bytes()?;
        let module = if Module::is_binary(&bytes) {
            Module::try_from(bytes.as_slice()).map_err(|e| format!("{file}: {e}"))?
        } else {
            let text = std::str::from_utf8(&bytes).map_err(|e| format!("{file}: {e}"))?;
            text.parse::<Module>().map_err(|e| format!("{file}: {e}"))?
        };
        self.io.module(&module, self.binary)
    }
}
