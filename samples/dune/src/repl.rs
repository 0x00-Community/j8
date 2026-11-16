use std::io::{BufRead, Write};
use std::process::ExitCode;

use j8framework::Source;

use crate::session::{Outcome, Session};

/// REPL commands starting with `.`.
#[derive(Clone, Copy)]
enum Command {
    Exit,
    Help,
}

impl Command {
    const TABLE: [(Self, &str, &str); 2] = [(Self::Exit, ".exit", "Exit"), (Self::Help, ".help", "List commands")];
}

impl TryFrom<&str> for Command {
    type Error = ();

    fn try_from(s: &str) -> Result<Self, ()> {
        Self::TABLE.iter().find(|(_, name, _)| *name == s).map(|(c, ..)| *c).ok_or(())
    }
}

impl Session<'_, '_> {
    /// One piece of source per line, all sharing one global object; ends at EOF (Ctrl-D) or `.exit`.
    pub fn repl(&mut self) -> ExitCode {
        println!("dune {} (j8). .help lists commands, Ctrl-D exits", env!("CARGO_PKG_VERSION"));
        let mut lines = std::io::stdin().lock().lines();
        loop {
            print!("> ");
            // Failing to write the prompt doesn't affect reading input
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
