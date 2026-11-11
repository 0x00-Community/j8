use std::process::ExitCode;

use r8builtins::Builtins;
use r8executor::Executor;
use r8frame::{ContextRoot, Error, Global, Isolate, Source};

/// 一段源码跑完的结果，都已按 ToString 转成文本。
pub enum Outcome {
    Value(String),
    /// 没接住的异常
    Thrown(String),
    /// 编不了（语法错 / 不支持的写法）
    Failed(String),
}

impl Outcome {
    /// 失败的结果打到标准错误，给出退出码。
    pub fn report(&self) -> ExitCode {
        match self {
            Outcome::Value(_) => return ExitCode::SUCCESS,
            Outcome::Thrown(e) => eprintln!("Uncaught {e}"),
            Outcome::Failed(e) => eprintln!("{e}"),
        }
        ExitCode::FAILURE
    }
}

/// 一个执行环境：多段源码共用同一个全局对象（REPL 一行一段），执行器与环境都活过多次进入。
pub struct Session<'s, 'p> {
    isolate: &'s mut Isolate<'p>,
    executor: Executor,
    cx: ContextRoot,
}

impl<'s, 'p> Session<'s, 'p> {
    /// 新环境：内建对象、node 式全局（`process` / `__dune`）、序言（`require`）。
    pub fn new(isolate: &'s mut Isolate<'p>, script: &str, args: &[String]) -> Self {
        let executor = Executor::default();
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

    /// 字符串字面量（JSON 写法）。
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

    /// 持久句柄里的值 ⇒ 文本（ToString；它自己再抛就给个占位）
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
