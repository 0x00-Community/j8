//! dune：r8 的 JavaScript 运行时，对标 node / deno。
//!
//! `dune a.js` / `dune run a.js` 跑文件，`dune -e '<js>'` 跑源码（`-p` 再打印结果），不带参数进 REPL。
mod node;
mod repl;
mod session;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use r8framework::{CreateParams, Isolate, Source};
use r8platform::DefaultPlatform;

use session::{Outcome, Session};

/// r8 的 JavaScript 运行时
#[derive(Parser)]
#[command(name = "dune", version, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// 要跑的脚本（同 `dune run`）；`-` 读标准输入
    file: Option<PathBuf>,
    /// 跑这段源码
    #[arg(short, long, conflicts_with_all = ["file", "print"])]
    eval: Option<String>,
    /// 跑这段源码并打印结果
    #[arg(short, long, conflicts_with = "file")]
    print: Option<String>,
    /// 开 JIT：热函数编成 x86-64 机器码跑（r8jit）
    #[arg(long)]
    jit: bool,
    /// 交给脚本的参数（`process.argv` 里脚本路径之后）
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

#[derive(Subcommand)]
enum Command {
    /// 跑一个脚本文件
    Run {
        /// 脚本；`-` 读标准输入
        file: PathBuf,
        /// 交给脚本的参数
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// 交互式 REPL（不带参数时的缺省）
    Repl,
}

/// 这次要做什么。
enum Job {
    /// 跑源码，`print` 时打印结果
    Script { text: String, file: String, print: bool },
    /// 跑脚本文件（按 CommonJS 模块，`require` / `module` / `__dirname` 可用）
    Main { file: String, args: Vec<String> },
    Repl,
}

impl TryFrom<Cli> for Job {
    type Error = String;

    fn try_from(cli: Cli) -> Result<Self, String> {
        let (file, args) = match (cli.command, cli.file, cli.eval, cli.print) {
            (Some(Command::Run { file, args }), ..) => (file, args),
            (None, Some(file), ..) => (file, cli.args),
            (Some(Command::Repl), ..) | (None, None, None, None) => return Ok(Job::Repl),
            (None, None, Some(text), _) => return Ok(Job::Script { text, file: "[eval]".into(), print: false }),
            (None, None, None, Some(text)) => return Ok(Job::Script { text, file: "[eval]".into(), print: true }),
        };
        if file.as_os_str() == "-" {
            let text = std::io::read_to_string(std::io::stdin()).map_err(|e| format!("[stdin]: {e}"))?;
            return Ok(Job::Script { text, file: "[stdin]".into(), print: false });
        }
        if !file.is_file() {
            return Err(format!("{}: no such file", file.display()));
        }
        Ok(Job::Main { file: file.display().to_string(), args })
    }
}

impl Job {
    /// 跑；`jit` 时普通 JS 函数热了编成机器码。
    fn run(&self, jit: bool) -> ExitCode {
        let platform = DefaultPlatform::default();
        let mut isolate = Isolate::new(&platform, CreateParams::default());
        let (script, args) = match self {
            Job::Main { file, args } => (file.as_str(), args.as_slice()),
            _ => ("[eval]", &[][..]),
        };
        let mut session = Session::new(&mut isolate, script, args, jit);
        match self {
            Job::Repl => session.repl(),
            Job::Main { file, .. } => {
                let call = format!("__dune.runMain({})", Session::quote(file));
                match session.eval(&Source { text: &call, file }) {
                    Outcome::Value(_) => ExitCode::SUCCESS,
                    failed => failed.report(),
                }
            }
            Job::Script { text, file, print } => match session.eval(&Source { text, file }) {
                Outcome::Value(v) => {
                    if *print {
                        println!("{v}");
                    }
                    ExitCode::SUCCESS
                }
                failed => failed.report(),
            },
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let jit = cli.jit;
    match Job::try_from(cli) {
        Ok(job) => job.run(jit),
        Err(e) => {
            eprintln!("dune: {e}");
            ExitCode::FAILURE
        }
    }
}
