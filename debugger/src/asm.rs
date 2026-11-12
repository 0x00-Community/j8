use std::io::{Read, Write};
use std::path::PathBuf;

use clap::Args;
use r8framework::Source;
use r8virtasm::Module;

/// 输入（文件 / 标准输入 / 命令行）与输出（文件 / 标准输出）。
#[derive(Args)]
pub struct Io {
    /// 输入文件；`-` 读标准输入
    #[arg(required_unless_present = "eval", conflicts_with = "eval")]
    file: Option<PathBuf>,
    /// 直接给输入文本
    #[arg(short, long)]
    eval: Option<String>,
    /// 写到文件，缺省标准输出
    #[arg(short, long)]
    output: Option<PathBuf>,
}

impl Io {
    /// 输入文本与报错用的文件名
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

/// `r8dbg disasm`：JS 编成字节码，打印反汇编清单。
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

/// `r8dbg asm`：汇编反汇编清单（`r8dbg disasm` 的输出），打印规范化后的清单。
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
