//! Building a runtime out of bound types: [`runtime!`](crate::runtime) declares a [`Host`] (globals, native modules,
//! hidden bindings, a JavaScript prelude, optionally on top of another host), [`HostPlatform`] serves its native
//! modules to `import`, and [`Runtime`] is a ready execution environment.
use std::marker::PhantomData;
use std::time::Duration;

use j8framework::{Agent, ContextRoot, CreateParams, Error, Global, Isolate, Platform, Pool, Source, Task};

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
pub struct HostPlatform<H, P> {
    inner: P,
    _host: PhantomData<fn() -> H>,
}

impl<H: Host, P: Platform> HostPlatform<H, P> {
    pub fn new(inner: P) -> Self {
        Self { inner, _host: PhantomData }
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
}

/// A ready execution environment: an isolate, an executor and one context with a host installed and its preludes run.
pub struct Runtime<'p> {
    isolate: Isolate<'p>,
    executor: j8executor::Executor,
    cx: ContextRoot,
}

impl<'p> Runtime<'p> {
    /// Builds the context, installs `H` and runs its preludes (the platform should be a [`HostPlatform`] for `H`, so
    /// `import` finds the native modules)
    pub fn new<H: Host>(platform: &'p dyn Platform, executor: j8executor::Executor) -> Result<Self, Error> {
        let mut isolate = Isolate::new(platform, CreateParams::default());
        let cx = isolate.enter(|scope| scope.persist(scope.context(&j8builtins::Builtins)));
        let mut rt = Self { isolate, executor, cx };
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

    pub fn isolate(&mut self) -> &mut Isolate<'p> {
        &mut self.isolate
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
