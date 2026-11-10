use std::process::ExitCode;

use r8builtins::Builtins;
use r8executor::Executor;
use r8frame::{Context, Error, Scope, Source, Value};

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

/// 一次进入 isolate 里的执行环境：多段源码共用同一个全局对象（REPL 一行一段）。
pub struct Session<'s, 'gc, 'i> {
    scope: &'s mut Scope<'gc, 'i>,
    executor: &'s mut Executor,
    cx: Context<'gc>,
}

impl<'s, 'gc, 'i> Session<'s, 'gc, 'i> {
    pub fn new(scope: &'s mut Scope<'gc, 'i>, executor: &'s mut Executor) -> Self {
        let cx = scope.context(&Builtins);
        Self { scope, executor, cx }
    }

    /// 值 ⇒ 文本（ToString；它自己再抛就给个占位）
    fn text(&mut self, v: Value<'gc>) -> String {
        let mut agent = self.scope.agent(self.cx, &*self.executor);
        agent.to_string(v).map_or_else(|_| String::from("<ToString threw>"), |s| String::from(&**s))
    }

    pub fn eval(&mut self, src: &Source<'_>) -> Outcome {
        match self.scope.run(self.cx, &mut *self.executor, src) {
            Ok(v) => Outcome::Value(self.text(v)),
            Err(Error::Exception(g)) => {
                let v = self.scope.local(&g);
                Outcome::Thrown(self.text(v))
            }
            Err(e) => Outcome::Failed(e.to_string()),
        }
    }
}
