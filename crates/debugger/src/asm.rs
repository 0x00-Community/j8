use std::io::{Read, Write};
use std::path::PathBuf;

use clap::Args;
use r8bytecode::Module;
use r8frame::Source;

/// `r8dbg asm`：把 JS 编成字节码并打印反汇编。
#[derive(Args)]
pub struct Asm {
    /// JS 文件；`-` 读标准输入
    #[arg(required_unless_present = "eval", conflicts_with = "eval")]
    file: Option<PathBuf>,
    /// 直接给 JS 源码
    #[arg(short, long)]
    eval: Option<String>,
    /// 写到文件，缺省标准输出
    #[arg(short, long)]
    output: Option<PathBuf>,
}

impl Asm {
    /// 源码与报错用的文件名
    fn source(&self) -> Result<(String, String), String> {
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

    pub fn run(&self) -> Result<(), String> {
        let (text, file) = self.source()?;
        let module = Module::try_from(&Source { text: &text, file: &file }).map_err(|e| e.to_string())?;
        let listing = module.to_string();
        match &self.output {
            Some(p) => std::fs::write(p, listing).map_err(|e| format!("{}: {e}", p.display())),
            None => std::io::stdout().lock().write_all(listing.as_bytes()).map_err(|e| e.to_string()),
        }
    }
}
