//! Node 专有部分里靠事件循环的两样：异步工作（工作线程算完回 JS 线程）、线程安全函数（别的线程送三次后释放）。
use core::ffi::{c_char, c_void};
use core::num::NonZeroUsize;
use core::ptr::{null, null_mut};

use j8builtins::Builtins;
use j8executor::Executor;
use j8framework::{EventLoopSet, ScriptKind, Source};
use j8napi::{CallbackInfo, Env, Handle, Status};
use j8platform::DefaultPlatform;

type Cb = Option<unsafe extern "C" fn(*mut Env, *mut CallbackInfo) -> Handle>;
type Execute = Option<unsafe extern "C" fn(*mut Env, *mut c_void)>;
type Complete = Option<unsafe extern "C" fn(*mut Env, Status, *mut c_void)>;
type CallJs = Option<unsafe extern "C" fn(*mut Env, Handle, *mut c_void, *mut c_void)>;

#[allow(improper_ctypes, reason = "插件只把 env / info / 工作 / 线程安全函数当不透明指针")]
unsafe extern "C" {
    fn napi_create_function(env: *mut Env, name: *const c_char, len: usize, cb: Cb, data: *mut c_void, result: *mut Handle) -> Status;
    fn napi_set_named_property(env: *mut Env, o: Handle, name: *const c_char, v: Handle) -> Status;
    fn napi_get_cb_info(env: *mut Env, info: *mut CallbackInfo, argc: *mut usize, argv: *mut Handle, this: *mut Handle, data: *mut *mut c_void) -> Status;
    fn napi_create_double(env: *mut Env, x: f64, result: *mut Handle) -> Status;
    fn napi_get_undefined(env: *mut Env, result: *mut Handle) -> Status;
    fn napi_call_function(env: *mut Env, recv: Handle, f: Handle, argc: usize, argv: *const Handle, result: *mut Handle) -> Status;
    fn napi_create_reference(env: *mut Env, v: Handle, count: u32, result: *mut *mut c_void) -> Status;
    fn napi_get_reference_value(env: *mut Env, r: *mut c_void, result: *mut Handle) -> Status;
    fn napi_delete_reference(env: *mut Env, r: *mut c_void) -> Status;
    fn napi_create_async_work(env: *mut Env, resource: Handle, name: Handle, execute: Execute, complete: Complete, data: *mut c_void, result: *mut *mut c_void) -> Status;
    fn napi_queue_async_work(env: *mut Env, work: *mut c_void) -> Status;
    fn napi_delete_async_work(env: *mut Env, work: *mut c_void) -> Status;
    fn napi_create_threadsafe_function(env: *mut Env, f: Handle, resource: Handle, name: Handle, max: usize, threads: usize, fin_data: *mut c_void, fin: Option<unsafe extern "C" fn(*mut Env, *mut c_void, *mut c_void)>, context: *mut c_void, call_js: CallJs, result: *mut *mut c_void) -> Status;
    fn napi_call_threadsafe_function(f: *mut c_void, data: *mut c_void, blocking: i32) -> Status;
    fn napi_release_threadsafe_function(f: *mut c_void, mode: i32) -> Status;
}

/// 1 + 2 + … + 1000
const SUM_TO: u32 = 1000;

/// 一件异步工作：工作线程上算的和、完成后要调的 JS 回调
struct Job {
    sum: f64,
    cb: *mut c_void,
    work: *mut c_void,
}

/// 第一个实参（回调）
unsafe fn first(env: *mut Env, info: *mut CallbackInfo) -> Handle {
    let (mut argc, mut argv) = (1usize, [Handle::default()]);
    unsafe { napi_get_cb_info(env, info, &raw mut argc, argv.as_mut_ptr(), null_mut(), null_mut()) };
    argv[0]
}

unsafe extern "C" fn execute(_: *mut Env, data: *mut c_void) {
    let job = unsafe { &mut *data.cast::<Job>() };
    job.sum = (1..=SUM_TO).map(f64::from).sum();
}

unsafe extern "C" fn complete(env: *mut Env, status: Status, data: *mut c_void) {
    assert_eq!(status, Status::Ok);
    let job = unsafe { Box::from_raw(data.cast::<Job>()) };
    let (mut f, mut this, mut x) = (Handle::default(), Handle::default(), Handle::default());
    unsafe {
        napi_get_reference_value(env, job.cb, &raw mut f);
        napi_get_undefined(env, &raw mut this);
        napi_create_double(env, job.sum, &raw mut x);
        napi_call_function(env, this, f, 1, &raw const x, null_mut());
        napi_delete_reference(env, job.cb);
        napi_delete_async_work(env, job.work);
    }
}

/// `work(cb)`：在工作线程上求和，回 JS 线程调 `cb(和)`
unsafe extern "C" fn work(env: *mut Env, info: *mut CallbackInfo) -> Handle {
    let cb = unsafe { first(env, info) };
    let job = Box::into_raw(Box::new(Job { sum: 0.0, cb: null_mut(), work: null_mut() }));
    unsafe {
        napi_create_reference(env, cb, 1, &raw mut (*job).cb);
        napi_create_async_work(env, Handle::default(), Handle::default(), Some(execute), Some(complete), job.cast(), &raw mut (*job).work);
        assert_eq!(napi_queue_async_work(env, (*job).work), Status::Ok);
    }
    Handle::default()
}

unsafe extern "C" fn call_js(env: *mut Env, f: Handle, _: *mut c_void, data: *mut c_void) {
    let (mut this, mut x) = (Handle::default(), Handle::default());
    unsafe {
        napi_get_undefined(env, &raw mut this);
        #[expect(clippy::cast_precision_loss, reason = "送的是 1、2、3")]
        napi_create_double(env, data.addr() as f64, &raw mut x);
        napi_call_function(env, this, f, 1, &raw const x, null_mut());
    }
}

/// 交给别的线程的线程安全函数
struct Remote(*mut c_void);

// SAFETY: 线程安全函数本来就是给别的线程用的
unsafe impl Send for Remote {}

/// `feed(cb)`：别的线程经线程安全函数送 1、2、3，然后释放
unsafe extern "C" fn feed(env: *mut Env, info: *mut CallbackInfo) -> Handle {
    let cb = unsafe { first(env, info) };
    let mut t = null_mut();
    unsafe { assert_eq!(napi_create_threadsafe_function(env, cb, Handle::default(), Handle::default(), 0, 1, null_mut(), None, null_mut(), Some(call_js), &raw mut t), Status::Ok) };
    let remote = Remote(t);
    std::thread::spawn(move || {
        let r = remote;
        for i in 1..=3usize {
            unsafe { napi_call_threadsafe_function(r.0, core::ptr::without_provenance_mut(i), 0) };
        }
        unsafe { napi_release_threadsafe_function(r.0, 0) };
    });
    Handle::default()
}

unsafe extern "C" fn register(env: *mut Env, exports: Handle) -> Handle {
    let (mut w, mut f) = (Handle::default(), Handle::default());
    unsafe {
        napi_create_function(env, null(), 0, Some(work), null_mut(), &raw mut w);
        napi_set_named_property(env, exports, c"work".as_ptr(), w);
        napi_create_function(env, null(), 0, Some(feed), null_mut(), &raw mut f);
        napi_set_named_property(env, exports, c"feed".as_ptr(), f);
    }
    exports
}

#[test]
fn async_work_and_threadsafe_functions() {
    let platform = DefaultPlatform::default();
    EventLoopSet::scope(&platform, NonZeroUsize::MIN, |set| {
        let l = set.spawn(Executor::default(), &Builtins);
        l.enter(|a| {
            let exports = Env::open(a, register).expect("register");
            a.set_named(a.realm.global.into(), "addon", exports).expect("global");
        });
        let start = "globalThis.got = []; addon.work(s => got.push(`sum ${s}`)); addon.feed(x => got.push(x));";
        l.eval(&Source { text: start, file: "start.js" }, ScriptKind::Classic).expect("start");
        l.run().expect("loop");
        let g = l.eval(&Source { text: "got.sort().join(' ')", file: "end.js" }, ScriptKind::Classic).expect("end");
        let text = l.enter(|a| {
            let v = a.local(&g);
            String::from(&**a.to_string(v).expect("ToString"))
        });
        assert_eq!(text, "1 2 3 sum 500500");
    });
}
