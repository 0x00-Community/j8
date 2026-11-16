use std::process::ExitCode;

use j8builtins::Builtins;
use j8executor::Executor;
use j8framework::{ContextRoot, Error, Global, Isolate, Source};

/// The result of running a piece of source, already converted to text via ToString.
pub enum Outcome {
    Value(String),
    /// Uncaught exception
    Thrown(String),
    /// Can't compile (syntax error / unsupported construct)
    Failed(String),
}

impl Outcome {
    /// Print a failed result to stderr and return the exit code.
    pub fn report(&self) -> ExitCode {
        match self {
            Outcome::Value(_) => return ExitCode::SUCCESS,
            Outcome::Thrown(e) => eprintln!("Uncaught {e}"),
            Outcome::Failed(e) => eprintln!("{e}"),
        }
        ExitCode::FAILURE
    }
}

/// An execution environment: multiple pieces of source share one global object (one per REPL line); the executor
/// and the environment both outlive multiple entries.
pub struct Session<'s, 'p> {
    isolate: &'s mut Isolate<'p>,
    executor: Executor,
    cx: ContextRoot,
}

impl<'s, 'p> Session<'s, 'p> {
    /// New environment: built-in objects, node-style globals (`process` / `__dune`), prelude (`require`).
    pub fn new(isolate: &'s mut Isolate<'p>, script: &str, args: &[String], jit: bool) -> Self {
        let executor = if jit { Executor::default().with_tier(std::rc::Rc::new(j8jit::Jit::default())) } else { Executor::default() };
        let cx = isolate.enter(|scope| scope.persist(scope.context(&Builtins)));
        isolate.enter(|scope| {
            let realm = scope.context_of(&cx);
            let mut a = scope.agent(realm, &executor);
            crate::node::Node::install(&mut a, script, args);
        });
        let mut s = Self { isolate, executor, cx };
        if let Outcome::Thrown(e) | Outcome::Failed(e) = s.eval(&Source { text: crate::node::PRELUDE, file: "[prelude]" }) {
            eprintln!("dune: prelude failed: {e}");
        }
        s
    }

    /// String literal (JSON syntax).
    pub fn quote(s: &str) -> String {
        let mut out = String::from("\"");
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    /// Value in a persistent handle ⇒ text (ToString; a placeholder if that throws in turn)
    fn text(&mut self, g: &Global) -> String {
        let (executor, cx) = (&self.executor, &self.cx);
        self.isolate.enter(|scope| {
            let realm = scope.context_of(cx);
            let mut agent = scope.agent(realm, executor);
            let v = agent.local(g);
            agent.to_string(v).map_or_else(|_| String::from("<ToString threw>"), |s| String::from(&**s))
        })
    }

    pub fn eval(&mut self, src: &Source<'_>) -> Outcome {
        match self.isolate.run(&mut self.executor, &self.cx, src) {
            Ok(g) => Outcome::Value(self.text(&g)),
            Err(Error::Exception(g)) => Outcome::Thrown(self.text(&g)),
            Err(e) => Outcome::Failed(e.to_string()),
        }
    }
}
