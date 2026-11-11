use std::io::{BufRead, Write};
use std::process::ExitCode;

use r8frame::Source;

use crate::session::{Outcome, Session};

/// REPL 里以 `.` 开头的命令。
#[derive(Clone, Copy)]
enum Command {
    Exit,
    Help,
}

impl Command {
    const TABLE: [(Self, &str, &str); 2] = [(Self::Exit, ".exit", "退出"), (Self::Help, ".help", "列出命令")];
}

impl TryFrom<&str> for Command {
    type Error = ();

    fn try_from(s: &str) -> Result<Self, ()> {
        Self::TABLE.iter().find(|(_, name, _)| *name == s).map(|(c, ..)| *c).ok_or(())
    }
}

impl Session<'_, '_> {
    /// 一行一段源码，全部共用同一个全局对象；读到 EOF（Ctrl-D）或 `.exit` 结束。
    pub fn repl(&mut self) -> ExitCode {
        println!("dune {}（r8）。.help 列出命令，Ctrl-D 退出", env!("CARGO_PKG_VERSION"));
        let mut lines = std::io::stdin().lock().lines();
        loop {
            print!("> ");
            // 提示符写不出去也不影响读输入
            let _ = std::io::stdout().flush();
            let Some(Ok(line)) = lines.next() else { break };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match Command::try_from(line) {
                Ok(Command::Exit) => break,
                Ok(Command::Help) => Command::TABLE.iter().for_each(|(_, name, doc)| println!("{name:<8}{doc}")),
                Err(()) => match self.eval(&Source { text: line, file: "[repl]" }) {
                    Outcome::Value(v) => println!("{v}"),
                    failed => {
                        failed.report();
                    }
                },
            }
        }
        ExitCode::SUCCESS
    }
}
