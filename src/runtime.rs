//! Building a runtime out of bound types: [`runtime!`](crate::runtime) declares a [`Host`] (globals, native modules,
//! hidden bindings, a JavaScript prelude, optionally on top of another host), [`HostPlatform`] serves its native
//! modules to `import`, and [`Runtime`] is a ready execution environment.
use std::marker::PhantomData;
use std::time::Duration;

use j8framework::{Agent, Error, ErrorKind, EventLoop, EventLoopSet, Global, Platform, Pool, ScriptKind, Source, Task, Value};

/// A runtime's contents (implemented by [`runtime!`](crate::runtime); a parent host's contents come first).
pub trait Host {
    /// Installs the globals, native modules and hidden bindings into a fresh realm
    fn install(a: &mut Agent<'_, '_>);

    /// The ES module source of a native module, if `specifier` names one
    fn module_source(specifier: &str) -> Option<String>;

    /// The JavaScript preludes to run, in order: (name, source)
    fn preludes(out: &mut Vec<(&'static str, &'static str)>);
}

/// A platform that answers a [`Host`]'s native module specifiers itself and passes everything else to `P`. It holds
/// no per-runtime state: several runtimes may share one (async host work is queued on each runtime's own isolate).
pub struct HostPlatform<H, P> {
    inner: P,
    _host: PhantomData<fn() -> H>,
}

impl<H: Host, P: Platform> HostPlatform<H, P> {
    /// Serves `H`'s native modules on top of `inner`
    pub fn new(inner: P) -> Self {
        Self { inner, _host: PhantomData }
    }

    /// The wrapped platform
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
}

/// A ready execution environment: one JS event loop ([`EventLoop`]: an isolate with one context, a host installed
/// and its preludes run) in an [`EventLoopSet`], on its least loaded loop thread. Any host thread may use it; each
/// call waits for the loop thread's answer.
pub struct Runtime<'p> {
    js: EventLoop<'p, j8executor::Executor>,
}

impl<'p> Runtime<'p> {
    /// A new event loop of `set` (on the least loaded of its loop threads): builds the context, installs `H` and runs
    /// its preludes (the set's platform should be a [`HostPlatform`] for `H`, so `import` finds the native modules)
    pub fn on<H: Host>(set: &EventLoopSet<'_, 'p, j8executor::Executor>, executor: j8executor::Executor) -> Result<Self, Error> {
        let rt = Self { js: set.spawn(executor, &j8builtins::Builtins) };
        rt.enter(|a| H::install(a));
        let mut preludes = Vec::new();
        H::preludes(&mut preludes);
        // The same preludes in every runtime: compiled once when the executor shares a code cache
        for (name, text) in preludes {
            rt.js.eval(&Source { text, file: name }, ScriptKind::Shared)?;
        }
        Ok(rt)
    }

    /// Runs a script in the context (the event loop goes on with its host work meanwhile)
    pub fn eval(&self, text: &str, file: &str) -> Result<Global, Error> {
        self.js.eval(&Source { text, file }, ScriptKind::Classic)
    }

    /// Runs an ES module in the context
    pub fn eval_module(&self, text: &str, file: &str) -> Result<Global, Error> {
        self.js.eval(&Source { text, file }, ScriptKind::Module)
    }

    /// Works with the context directly (installing more, reading results), on the event loop's thread
    pub fn enter<R: Send>(&self, f: impl for<'gc> FnOnce(&mut Agent<'gc, '_>) -> R + Send) -> R {
        self.js.enter(f)
    }

    /// ToString of a handle's value (a placeholder when that throws in turn)
    pub fn text(&self, g: &Global) -> String {
        self.enter(|a| {
            let v = a.local(g);
            a.to_string(v).map_or_else(|_| String::from("<ToString threw>"), |s| String::from(&**s))
        })
    }

    /// Defines `value` (converted with `IntoJs`) as a data property at a dotted path from the global object:
    /// `rt.set("process.argv", args)` sets `argv` on the existing global `process`
    pub fn set<V: for<'g> crate::IntoJs<'g> + Send>(&self, path: &str, value: V) -> Result<(), Error> {
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

    /// The event loop: runs until all scripts and async host work have finished (settlements run the microtasks
    /// they queue), sleeping while nothing can make progress. Work spawned meanwhile joins in.
    pub fn run_until_idle(&self) -> Result<(), Error> {
        self.js.run()
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

            /// A ready runtime in `set` (see [`Runtime::on`]($crate::Runtime::on))
            pub fn runtime<'p>(set: &$crate::EventLoopSet<'_, 'p, $crate::executor::Executor>, executor: $crate::executor::Executor) -> ::core::result::Result<$crate::Runtime<'p>, $crate::frame::Error> {
                $crate::Runtime::on::<$name>(set, executor)
            }
        }
    };
}
