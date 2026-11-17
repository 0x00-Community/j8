//! Building a runtime out of bound types: [`runtime!`](crate::runtime) declares a [`Host`] (globals, native modules,
//! hidden bindings, a JavaScript prelude, optionally on top of another host), [`HostPlatform`] serves its native
//! modules to `import`, and [`Runtime`] is a ready execution environment.
use std::marker::PhantomData;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::Thread;
use std::time::Duration;

use j8framework::{Agent, AsyncJob, ContextRoot, CreateParams, Error, ErrorKind, Global, Isolate, Platform, Pool, Source, Task, Value};
use j8lockfree::Fifo;

/// A runtime's contents (implemented by [`runtime!`](crate::runtime); a parent host's contents come first).
pub trait Host {
    /// Installs the globals, native modules and hidden bindings into a fresh realm
    fn install(a: &mut Agent<'_, '_>);

    /// The ES module source of a native module, if `specifier` names one
    fn module_source(specifier: &str) -> Option<String>;

    /// The JavaScript preludes to run, in order: (name, source)
    fn preludes(out: &mut Vec<(&'static str, &'static str)>);
}

/// A platform that answers a [`Host`]'s native module specifiers itself and passes everything else to `P`.
/// It also queues the async host work (`async fn`s bound with the binding macros) for [`Runtime::run_until_idle`].
pub struct HostPlatform<H, P> {
    inner: P,
    spawned: Fifo<AsyncJob>,
    _host: PhantomData<fn() -> H>,
}

impl<H: Host, P: Platform> HostPlatform<H, P> {
    pub fn new(inner: P) -> Self {
        Self { inner, spawned: Fifo::default(), _host: PhantomData }
    }

    pub fn inner(&self) -> &P {
        &self.inner
    }
}

impl<H, P: Pool> Pool for HostPlatform<H, P> {
    fn threads(&self) -> usize {
        self.inner.threads()
    }

    fn run(&self, jobs: Vec<Task<'_>>) {
        self.inner.run(jobs);
    }
}

impl<H: Host, P: Platform> Platform for HostPlatform<H, P> {
    fn now(&self) -> Duration {
        self.inner.now()
    }

    fn wall_clock(&self) -> Duration {
        self.inner.wall_clock()
    }

    fn load_module(&self, referrer: &str, specifier: &str) -> Option<(String, String)> {
        match H::module_source(specifier) {
            Some(text) => Some((specifier.to_owned(), text)),
            None => self.inner.load_module(referrer, specifier),
        }
    }

    fn print(&self, line: &str) {
        self.inner.print(line);
    }

    fn spawn(&self, job: AsyncJob) -> Result<(), AsyncJob> {
        self.spawned.push(job);
        Ok(())
    }

    fn take_spawned(&self) -> Vec<AsyncJob> {
        core::iter::from_fn(|| self.spawned.pop()).collect()
    }
}

/// Wakes the runtime's thread when a future it polls can make progress.
struct Unpark(Thread);

impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// A ready execution environment: an isolate, an executor and one context with a host installed and its preludes run.
pub struct Runtime<'p> {
    platform: &'p dyn Platform,
    isolate: Isolate<'p>,
    executor: j8executor::Executor,
    cx: ContextRoot,
    /// Async host work not settled yet
    pending: Vec<AsyncJob>,
}

impl<'p> Runtime<'p> {
    /// Builds the context, installs `H` and runs its preludes (the platform should be a [`HostPlatform`] for `H`, so
    /// `import` finds the native modules)
    pub fn new<H: Host>(platform: &'p dyn Platform, executor: j8executor::Executor) -> Result<Self, Error> {
        let mut isolate = Isolate::new(platform, CreateParams::default());
        let cx = isolate.enter(|scope| scope.persist(scope.context(&j8builtins::Builtins)));
        let mut rt = Self { platform, isolate, executor, cx, pending: Vec::new() };
        rt.enter(|a| H::install(a));
        let mut preludes = Vec::new();
        H::preludes(&mut preludes);
        for (name, text) in preludes {
            rt.eval(text, name)?;
        }
        Ok(rt)
    }

    /// Runs a script in the context
    pub fn eval(&mut self, text: &str, file: &str) -> Result<Global, Error> {
        self.isolate.run(&mut self.executor, &self.cx, &Source { text, file })
    }

    /// Runs an ES module in the context
    pub fn eval_module(&mut self, text: &str, file: &str) -> Result<Global, Error> {
        self.isolate.run_module(&mut self.executor, &self.cx, &Source { text, file })
    }

    /// Works with the context directly (installing more, reading results)
    pub fn enter<R>(&mut self, f: impl for<'gc> FnOnce(&mut Agent<'gc, '_>) -> R) -> R {
        let (executor, cx) = (&self.executor, &self.cx);
        self.isolate.enter(|scope| {
            let realm = scope.context_of(cx);
            let mut a = scope.agent(realm, executor);
            f(&mut a)
        })
    }

    /// ToString of a handle's value (a placeholder when that throws in turn)
    pub fn text(&mut self, g: &Global) -> String {
        self.enter(|a| {
            let v = a.local(g);
            a.to_string(v).map_or_else(|_| String::from("<ToString threw>"), |s| String::from(&**s))
        })
    }

    /// Defines `value` (converted with `IntoJs`) as a data property at a dotted path from the global object:
    /// `rt.set("process.argv", args)` sets `argv` on the existing global `process`
    pub fn set<V: for<'g> crate::IntoJs<'g>>(&mut self, path: &str, value: V) -> Result<(), Error> {
        self.enter(|a| {
            let (holder, key) = path.rsplit_once('.').unwrap_or(("", path));
            let mut target: Value<'_> = a.realm.global.into();
            for part in holder.split('.').filter(|p| !p.is_empty()) {
                target = a.get_named(target, part)?;
            }
            let Some(o) = target.as_object() else { return Err(a.throw(ErrorKind::Type, &format!("{holder} is not an object"))) };
            let v = value.into_js(a)?;
            o.borrow_mut(a.mc).define(key, j8framework::Property::data(v));
            Ok(())
        })
    }

    pub fn isolate(&mut self) -> &mut Isolate<'p> {
        &mut self.isolate
    }

    /// The event loop: polls the async host work until all of it has settled its promises (running the microtasks
    /// each settlement queues), parking the thread while nothing can make progress. Work spawned meanwhile joins in.
    pub fn run_until_idle(&mut self) -> Result<(), Error> {
        let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
        let mut cx = Context::from_waker(&waker);
        loop {
            self.pending.extend(self.platform.take_spawned());
            if self.pending.is_empty() {
                return Ok(());
            }
            let mut progressed = false;
            let mut i = 0;
            while i < self.pending.len() {
                match self.pending[i].future.as_mut().poll(&mut cx) {
                    Poll::Ready(settle) => {
                        let job = self.pending.remove(i);
                        self.enter(|a| Self::settle(a, &job, settle))?;
                        progressed = true;
                    }
                    Poll::Pending => i += 1,
                }
            }
            if !progressed {
                std::thread::park();
            }
        }
    }

    /// Resolves (or rejects) a job's promise with what it settled to, then runs the microtasks
    fn settle(a: &mut Agent<'_, '_>, job: &AsyncJob, settle: j8framework::Settle) -> Result<(), Error> {
        let (target, value) = match settle(a) {
            Ok(v) => (&job.resolve, v),
            Err(Error::Exception(g)) => (&job.reject, a.local(&g)),
            Err(e) => {
                let message = e.to_string();
                let Error::Exception(g) = a.throw(ErrorKind::Error, &message) else { unreachable!("throw makes an exception") };
                (&job.reject, a.local(&g))
            }
        };
        let f = a.local(target);
        a.call(f, Value::Undefined, &[value])?;
        a.run_jobs()
    }
}

/// Declares a runtime: `j8::runtime! { pub Dune { globals: [Process], modules: [Fs, Paths], natives: [], prelude:
/// PRELUDE, } }`, or on top of another one: `pub MyNode: Dune { modules: [Sqlite], }`. Every section is optional and
/// they come in this order, each followed by a comma:
///
/// - `globals`: types bound with `#[j8::class]` / `#[j8::object]`, installed on the global object
/// - `modules`: types bound with `#[j8::module("…")]`, served to `import` and kept in `globalThis.__j8_native`
/// - `natives`: bound types the prelude uses, installed on `globalThis.__j8_native` (not global)
/// - `prelude`: JavaScript run after installing (a parent's prelude runs first)
#[macro_export]
macro_rules! runtime {
    (
        $(#[$meta:meta])* $vis:vis $name:ident $(: $parent:ty)? {
            $(globals: [$($global:ty),* $(,)?],)?
            $(modules: [$($module:ty),* $(,)?],)?
            $(natives: [$($native:ty),* $(,)?],)?
            $(prelude: $prelude:expr,)?
        }
    ) => {
        $(#[$meta])*
        $vis struct $name;

        impl $crate::Host for $name {
            fn install(a: &mut $crate::frame::Agent<'_, '_>) {
                $(<$parent as $crate::Host>::install(a);)?
                let global = a.realm.global;
                $($(<$global as $crate::Install>::install_into(a, global);)*)?
                $($($crate::Glue::register_module::<$module>(a);)*)?
                $(
                    let natives = $crate::Glue::natives(a);
                    $(<$native as $crate::Install>::install_into(a, natives);)*
                )?
                let _ = global;
            }

            fn module_source(specifier: &str) -> ::core::option::Option<::std::string::String> {
                $($(
                    if let ::core::option::Option::Some(s) = $crate::Glue::module_source::<$module>(specifier) {
                        return ::core::option::Option::Some(s);
                    }
                )*)?
                $(
                    if let ::core::option::Option::Some(s) = <$parent as $crate::Host>::module_source(specifier) {
                        return ::core::option::Option::Some(s);
                    }
                )?
                let _ = specifier;
                ::core::option::Option::None
            }

            fn preludes(out: &mut ::std::vec::Vec<(&'static str, &'static str)>) {
                $(<$parent as $crate::Host>::preludes(out);)?
                $(out.push((::core::concat!("[", ::core::stringify!($name), " prelude]"), $prelude));)?
                let _ = out;
            }
        }

        impl $name {
            /// A platform serving this runtime's native modules on top of `inner`
            pub fn platform<P: $crate::frame::Platform>(inner: P) -> $crate::HostPlatform<$name, P> {
                $crate::HostPlatform::new(inner)
            }

            /// A ready runtime on `platform` (see [`Runtime::new`]($crate::Runtime::new))
            pub fn runtime<'p>(platform: &'p dyn $crate::frame::Platform, executor: $crate::executor::Executor) -> ::core::result::Result<$crate::Runtime<'p>, $crate::frame::Error> {
                $crate::Runtime::new::<$name>(platform, executor)
            }
        }
    };
}
