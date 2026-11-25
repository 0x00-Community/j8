use core::ffi::{c_char, c_void};
use core::ptr::{null, null_mut};

use j8builtins::Builtins;
use j8executor::Executor;
use j8framework::{CreateParams, Isolate, Source};
use j8napi::{Attributes, CallbackInfo, Env, Handle, PropertyDescriptor, Status};
use j8platform::DefaultPlatform;

type Cb = Option<unsafe extern "C" fn(*mut Env, *mut CallbackInfo) -> Handle>;
type Fin = Option<unsafe extern "C" fn(*mut Env, *mut c_void, *mut c_void)>;

#[allow(improper_ctypes, reason = "the addon treats env / info as opaque pointers")]
unsafe extern "C" {
    fn napi_create_function(env: *mut Env, name: *const c_char, len: usize, cb: Cb, data: *mut c_void, result: *mut Handle) -> Status;
    fn napi_define_class(env: *mut Env, name: *const c_char, len: usize, ctor: Cb, data: *mut c_void, n: usize, props: *const PropertyDescriptor, result: *mut Handle) -> Status;
    fn napi_get_cb_info(env: *mut Env, info: *mut CallbackInfo, argc: *mut usize, argv: *mut Handle, this: *mut Handle, data: *mut *mut c_void) -> Status;
    fn napi_get_value_double(env: *mut Env, v: Handle, result: *mut f64) -> Status;
    fn napi_create_double(env: *mut Env, x: f64, result: *mut Handle) -> Status;
    fn napi_set_named_property(env: *mut Env, o: Handle, name: *const c_char, v: Handle) -> Status;
    fn napi_wrap(env: *mut Env, o: Handle, native: *mut c_void, fin: Fin, hint: *mut c_void, result: *mut *mut c_void) -> Status;
    fn napi_unwrap(env: *mut Env, o: Handle, result: *mut *mut c_void) -> Status;
    fn napi_throw_error(env: *mut Env, code: *const c_char, msg: *const c_char) -> Status;
    fn napi_get_value_string_utf16(env: *mut Env, v: Handle, buf: *mut u16, bufsize: usize, result: *mut usize) -> Status;
    fn napi_create_string_utf16(env: *mut Env, s: *const u16, len: usize, result: *mut Handle) -> Status;
    fn napi_get_value_string_latin1(env: *mut Env, v: Handle, buf: *mut c_char, bufsize: usize, result: *mut usize) -> Status;
    fn napi_create_string_latin1(env: *mut Env, s: *const c_char, len: usize, result: *mut Handle) -> Status;
}

struct Count(f64);

unsafe extern "C" fn add(env: *mut Env, info: *mut CallbackInfo) -> Handle {
    let (mut argc, mut argv) = (2usize, [Handle::default(); 2]);
    let (mut x, mut y, mut r) = (0.0, 0.0, Handle::default());
    unsafe {
        napi_get_cb_info(env, info, &raw mut argc, argv.as_mut_ptr(), null_mut(), null_mut());
        napi_get_value_double(env, argv[0], &raw mut x);
        napi_get_value_double(env, argv[1], &raw mut y);
        napi_create_double(env, x + y, &raw mut r);
    }
    r
}

unsafe extern "C" fn free_count(_: *mut Env, data: *mut c_void, _: *mut c_void) {
    drop(unsafe { Box::from_raw(data.cast::<Count>()) });
}

unsafe extern "C" fn counter_new(env: *mut Env, info: *mut CallbackInfo) -> Handle {
    let mut this = Handle::default();
    unsafe {
        napi_get_cb_info(env, info, null_mut(), null_mut(), &raw mut this, null_mut());
        napi_wrap(env, this, Box::into_raw(Box::new(Count(0.0))).cast(), Some(free_count), null_mut(), null_mut());
    }
    this
}

unsafe extern "C" fn counter_inc(env: *mut Env, info: *mut CallbackInfo) -> Handle {
    let (mut this, mut p, mut r) = (Handle::default(), null_mut::<c_void>(), Handle::default());
    unsafe {
        napi_get_cb_info(env, info, null_mut(), null_mut(), &raw mut this, null_mut());
        napi_unwrap(env, this, &raw mut p);
        let c = &mut *p.cast::<Count>();
        c.0 += 1.0;
        napi_create_double(env, c.0, &raw mut r);
    }
    r
}

unsafe extern "C" fn boom(env: *mut Env, _: *mut CallbackInfo) -> Handle {
    unsafe { napi_throw_error(env, c"E_BOOM".as_ptr(), c"boom".as_ptr()) };
    Handle::default()
}

unsafe extern "C" fn utf16(env: *mut Env, info: *mut CallbackInfo) -> Handle {
    let (mut argc, mut arg, mut r) = (1usize, Handle::default(), Handle::default());
    let mut units = [0u16; 16];
    let mut n = 0;
    unsafe {
        napi_get_cb_info(env, info, &raw mut argc, &raw mut arg, null_mut(), null_mut());
        napi_get_value_string_utf16(env, arg, units.as_mut_ptr(), units.len(), &raw mut n);
        napi_create_string_utf16(env, units.as_ptr(), n, &raw mut r);
    }
    r
}

unsafe extern "C" fn latin1(env: *mut Env, info: *mut CallbackInfo) -> Handle {
    let (mut argc, mut arg, mut r) = (1usize, Handle::default(), Handle::default());
    let mut bytes = [0 as c_char; 16];
    let mut n = 0;
    unsafe {
        napi_get_cb_info(env, info, &raw mut argc, &raw mut arg, null_mut(), null_mut());
        napi_get_value_string_latin1(env, arg, bytes.as_mut_ptr(), bytes.len(), &raw mut n);
        napi_create_string_latin1(env, bytes.as_ptr(), n, &raw mut r);
    }
    r
}

unsafe extern "C" fn register(env: *mut Env, exports: Handle) -> Handle {
    let (mut f, mut g, mut class) = (Handle::default(), Handle::default(), Handle::default());
    let inc = PropertyDescriptor {
        utf8name: c"inc".as_ptr(),
        name: Handle::default(),
        method: Some(counter_inc),
        getter: None,
        setter: None,
        value: Handle::default(),
        attributes: Attributes(Attributes::WRITABLE | Attributes::CONFIGURABLE),
        data: null_mut(),
    };
    unsafe {
        assert_eq!(napi_create_function(env, c"add".as_ptr(), usize::MAX, Some(add), null_mut(), &raw mut f), Status::Ok);
        napi_set_named_property(env, exports, c"add".as_ptr(), f);
        napi_create_function(env, null(), 0, Some(boom), null_mut(), &raw mut g);
        napi_set_named_property(env, exports, c"boom".as_ptr(), g);
        assert_eq!(napi_define_class(env, c"Counter".as_ptr(), usize::MAX, Some(counter_new), null_mut(), 1, &raw const inc, &raw mut class), Status::Ok);
        napi_set_named_property(env, exports, c"Counter".as_ptr(), class);
        for (name, cb) in [(c"utf16", utf16 as unsafe extern "C" fn(*mut Env, *mut CallbackInfo) -> Handle), (c"latin1", latin1)] {
            let mut h = Handle::default();
            napi_create_function(env, name.as_ptr(), usize::MAX, Some(cb), null_mut(), &raw mut h);
            napi_set_named_property(env, exports, name.as_ptr(), h);
        }
    }
    exports
}

#[test]
fn functions_classes_and_errors() {
    let platform = DefaultPlatform::default();
    let mut isolate = Isolate::new(&platform, CreateParams::default());
    let executor = Executor::default();
    let cx = isolate.create_context(&Builtins);
    isolate.enter(&cx, &executor, |a| {
        let exports = Env::open(a, register).expect("register");
        a.set_named(a.context.global.into(), "addon", exports).expect("global");
    });
    let text = "
        const { add, Counter, boom, utf16, latin1 } = addon;
        class Sub extends Counter { twice() { this.inc(); return this.inc(); } }
        const c = new Counter(); c.inc();
        let caught = '';
        try { boom(); } catch (e) { caught = `${e.code}:${e.message}:${e instanceof Error}`; }
        const lone = utf16('a\\uD800b\\uDC00');
        const units = [...Array(lone.length).keys()].map((i) => lone.charCodeAt(i).toString(16)).join(',');
        const bytes = [...latin1('\\uD800\\u00E9')].map((ch) => ch.charCodeAt(0).toString(16)).join(',');
        [add(2, 3), c.inc(), new Sub().twice(), new Sub() instanceof Counter, Counter.name, caught, units, bytes].join(' ')
    ";
    let r = executor.eval(&mut isolate, &cx, &Source { text, file: "addon.js" }).expect("run");
    let got = isolate.enter(&cx, &executor, |a| {
        let v = a.local(&r);
        String::from(&**a.to_string(v).expect("ToString"))
    });
    assert_eq!(got, "5 2 2 true Counter E_BOOM:boom:true 61,d800,62,dc00 0,e9");
}
