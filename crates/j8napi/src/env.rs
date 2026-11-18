//! 一个插件实例的 Node-API 环境（`napi_env`）。
//!
//! - 句柄：`napi_value` 是句柄栈的下标加一，栈里存 [`Value::bits`]。句柄只在一次进入（`Isolate::enter`）里用，期间不回收，
//!   所以不用另外当根。
//! - 重入：C 回调里调 napi 函数会进 JS，JS 又会调回别的 napi 回调，所以 API 只拿 `&Env`，可变状态都在 `Cell` / `RefCell`
//!   里，借用不跨 JS 调用。
//! - 当前 Agent：进回调时记下指针（擦掉生命周期），API 用调用方选的 `'gc` 取回；不在 JS 里时为空，要跑 JS 的 API 返回
//!   [`Status::CannotRunJs`]。
//! - 终结回调：宿主对象被回收（可能在回收线程上）时把回调推进无锁队列，下次进回调时在 JS 线程上跑。
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::cell::{Cell, RefCell, UnsafeCell};
use core::ffi::c_void;
use core::num::NonZeroUsize;
use core::ptr::NonNull;

use j8framework::{Agent, Error, Global, JsObject, JsSymbol, ObjectData, ObjectKind, Property, Static, SymbolData, Value, ValueKind};
use j8lockfree::{AtomicRefCell, Fifo};

use crate::{Finalize, Handle, Status};

/// 句柄作用域（`napi_handle_scope` / `napi_escapable_handle_scope`）
#[derive(Clone, Copy, Debug)]
struct Scope {
    /// 开作用域时句柄栈的高度：关的时候退回这里
    base: usize,
    /// 可逃逸作用域预留在作用域外的槽，和是否已经逃逸过
    escape: Option<(usize, bool)>,
}

/// 待跑的终结回调
#[derive(Clone, Copy, Debug)]
pub struct Pending {
    pub cb: unsafe extern "C" fn(env: *mut Env, data: *mut c_void, hint: *mut c_void),
    pub data: *mut c_void,
    pub hint: *mut c_void,
}

// SAFETY: 只是转交给 JS 线程去调的指针，Node-API 规定插件自己保证它们能跨线程交出去
unsafe impl Send for Pending {}
// SAFETY: 同上
unsafe impl Sync for Pending {}

/// 一个宿主对象被回收时要跑的终结回调：drop 时推进队列（`None` 是已撤销，如 `napi_remove_wrap`）
pub struct Finalizer {
    pub queue: Arc<Fifo<Pending>>,
    pub pending: Option<Pending>,
}

impl Drop for Finalizer {
    fn drop(&mut self) {
        if let Some(p) = self.pending.take() {
            self.queue.push(p);
        }
    }
}

/// 插件的实例数据（`napi_set_instance_data`）
#[derive(Debug)]
struct Instance {
    data: *mut c_void,
    finalize: Option<Pending>,
}

/// ⭐ 一个插件实例的环境（`napi_env`）。
pub struct Env {
    /// 当前进入的 `Agent`（擦掉生命周期的指针）；不在 JS 里时为空
    agent: Cell<Option<NonNull<c_void>>>,
    handles: RefCell<Vec<u64>>,
    scopes: RefCell<Vec<Scope>>,
    /// 挂起的异常（napi 函数抛的、或调 JS 时冒出来的），回调返回时交给 JS
    pending: RefCell<Option<Error>>,
    /// 上一次调用的错误信息（指针交给 C，下次调用前有效）
    last: UnsafeCell<crate::ExtendedErrorInfo>,
    /// `napi_wrap` / 类型标签存放的私有名
    wrap_key: Global,
    tag_key: Global,
    instance: RefCell<Option<Instance>>,
    finalizers: Arc<Fifo<Pending>>,
    /// `napi_adjust_external_memory` 的累计
    external_memory: Cell<i64>,
    /// 清理钩子（`napi_add_env_cleanup_hook`），拆掉时倒序跑
    cleanup: RefCell<Vec<(crate::CleanupHook, usize)>>,
    /// `napi_get_node_version` 给出的版本（指针要一直有效）
    node_version: crate::NodeVersion,
}

// SAFETY: Env 只在它所属 isolate 的 JS 线程上用（裸指针都是那个线程的）；跨线程只有终结队列（无锁）
unsafe impl Send for Env {}
// SAFETY: 同上
unsafe impl Sync for Env {}

/// 宿主对象里装的 Env（随 global 对象一起活）
struct Holder(#[expect(dead_code, reason = "只为随宿主对象一起 drop")] Box<Env>);

impl Env {
    /// 当前 Agent。
    ///
    /// # Errors
    /// 不在 JS 里（终结回调跑在 isolate 拆掉时）：[`Status::CannotRunJs`]
    pub fn agent<'a, 'gc>(&self) -> Result<&'a mut Agent<'gc, 'a>, Status> {
        let p = self.agent.get().ok_or(Status::CannotRunJs)?;
        // SAFETY: `enter` 记下的指针，那次进入还没结束（`enter` 返回前清掉）；句柄与 API 只在这次进入里用
        Ok(unsafe { p.cast::<Agent<'gc, 'a>>().as_mut() })
    }

    /// 以 `a` 为当前 Agent 跑 `f`：期间开的句柄、作用域在返回时清掉，之前的 Agent 恢复（重入）。
    pub fn enter<R>(&self, a: &mut Agent<'_, '_>, f: impl FnOnce(&Self) -> R) -> R {
        let outer = self.agent.replace(Some(NonNull::from(a).cast()));
        let (handles, scopes) = (self.handles.borrow().len(), self.scopes.borrow().len());
        let r = f(self);
        self.handles.borrow_mut().truncate(handles);
        self.scopes.borrow_mut().truncate(scopes);
        self.agent.set(outer);
        r
    }

    /// 给 C 的指针
    pub fn as_ptr(&self) -> *mut Self {
        core::ptr::from_ref(self).cast_mut()
    }

    // ---- 句柄 ----

    /// 压一个值，给出它的句柄
    pub fn push(&self, v: Value<'_>) -> Handle {
        let mut h = self.handles.borrow_mut();
        h.push(v.bits());
        Handle(NonZeroUsize::new(h.len()))
    }

    /// 句柄里的值。
    ///
    /// # Errors
    /// 空句柄或不在栈里：[`Status::InvalidArg`]
    pub fn value<'gc>(&self, h: Handle) -> Result<Value<'gc>, Status> {
        let at = h.0.ok_or(Status::InvalidArg)?.get() - 1;
        let bits = *self.handles.borrow().get(at).ok_or(Status::InvalidArg)?;
        // SAFETY: 这次进入里由 `push` 压的值，期间不回收
        Ok(unsafe { Value::from_bits(bits) })
    }

    /// 写一个句柄到 `out`
    pub fn out(&self, out: *mut Handle, v: Value<'_>) -> Result<(), Status> {
        let h = self.push(v);
        crate::convert::Ffi::put(out, h)
    }

    /// 打开一个句柄作用域，给出它的编号（`napi_handle_scope`）
    pub fn open_scope(&self, escapable: bool) -> *mut c_void {
        let escape = escapable.then(|| {
            let mut h = self.handles.borrow_mut();
            h.push(Value::Undefined.bits());
            (h.len() - 1, false)
        });
        let base = self.handles.borrow().len();
        let mut s = self.scopes.borrow_mut();
        s.push(Scope { base, escape });
        core::ptr::without_provenance_mut(s.len())
    }

    /// 关掉最里层的作用域（`scope` 必须是它）
    pub fn close_scope(&self, scope: *mut c_void) -> Result<(), Status> {
        let mut s = self.scopes.borrow_mut();
        if scope.addr() != s.len() || s.is_empty() {
            return Err(Status::HandleScopeMismatch);
        }
        let top = s.pop().ok_or(Status::HandleScopeMismatch)?;
        self.handles.borrow_mut().truncate(top.base);
        Ok(())
    }

    /// 把 `v` 逃逸到作用域外预留的槽
    pub fn escape(&self, scope: *mut c_void, v: Value<'_>) -> Result<Handle, Status> {
        let mut s = self.scopes.borrow_mut();
        let sc = s.get_mut(scope.addr().checked_sub(1).ok_or(Status::InvalidArg)?).ok_or(Status::InvalidArg)?;
        let (slot, escaped) = sc.escape.as_mut().ok_or(Status::InvalidArg)?;
        if *escaped {
            return Err(Status::EscapeCalledTwice);
        }
        *escaped = true;
        let at = *slot;
        *self.handles.borrow_mut().get_mut(at).ok_or(Status::InvalidArg)? = v.bits();
        Ok(Handle(NonZeroUsize::new(at + 1)))
    }

    // ---- 错误 ----

    /// 调用的结尾：记下错误信息，给出返回码
    pub fn done(&self, r: Result<(), Status>) -> Status {
        let status = r.err().unwrap_or(Status::Ok);
        // SAFETY: 只在 JS 线程上、不跨调用地改
        let last = unsafe { &mut *self.last.get() };
        last.error_code = status;
        last.error_message = if status == Status::Ok { core::ptr::null() } else { status.message().as_ptr() };
        status
    }

    /// 上一次调用的错误信息
    pub fn last_error(&self) -> *const crate::ExtendedErrorInfo {
        self.last.get()
    }

    /// 已经有挂起的异常就不再跑 JS（`NAPI_PREAMBLE`）
    pub fn preamble(&self) -> Result<(), Status> {
        if self.pending.borrow().is_some() { Err(Status::PendingException) } else { Ok(()) }
    }

    /// 调 JS 的结果：异常挂起来，返回 [`Status::PendingException`]
    pub fn js<T>(&self, r: Result<T, Error>) -> Result<T, Status> {
        r.map_err(|e| {
            *self.pending.borrow_mut() = Some(e);
            Status::PendingException
        })
    }

    /// 抛 `e`（挂起，等回调返回时交给 JS）
    pub fn throw(&self, e: Error) {
        *self.pending.borrow_mut() = Some(e);
    }

    /// 取走挂起的异常
    pub fn take_pending(&self) -> Option<Error> {
        self.pending.borrow_mut().take()
    }

    pub fn has_pending(&self) -> bool {
        self.pending.borrow().is_some()
    }

    // ---- 私有名、宿主对象 ----

    /// `napi_wrap` 的私有名
    pub fn wrap_key<'gc>(&self, a: &Agent<'gc, '_>) -> JsSymbol<'gc> {
        Self::symbol(a, &self.wrap_key)
    }

    /// 类型标签的私有名
    pub fn tag_key<'gc>(&self, a: &Agent<'gc, '_>) -> JsSymbol<'gc> {
        Self::symbol(a, &self.tag_key)
    }

    fn symbol<'gc>(a: &Agent<'gc, '_>, g: &Global) -> JsSymbol<'gc> {
        match a.local(g).kind() {
            ValueKind::Symbol(s) => s,
            _ => unreachable!("the keys are symbols"),
        }
    }

    /// 一个装着 `v` 的宿主对象（没有原型）
    pub fn host<'gc>(a: &Agent<'gc, '_>, v: impl Any + Send + Sync) -> JsObject<'gc> {
        let data = a.mc.alloc(Static(AtomicRefCell::new(Box::new(v) as Box<dyn Any + Send + Sync>)));
        ObjectData { kind: ObjectKind::Host(data), proto: None, ..ObjectData::default() }.alloc(a.mc)
    }

    /// 宿主对象里的 `T`，交给 `f`
    pub fn hosted<T: Any, R>(o: JsObject<'_>, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        let ObjectKind::Host(h) = o.borrow().kind else { return None };
        let mut cell = h.0.try_borrow_mut()?;
        cell.downcast_mut::<T>().map(f)
    }

    /// `o` 的私有成员 `key`
    pub fn private<'gc>(o: JsObject<'gc>, key: JsSymbol<'gc>) -> Option<Value<'gc>> {
        o.borrow().privates.iter().find_map(|&(k, p)| (j8framework::Gc::ptr_eq(k, key)).then_some(p)).and_then(|p| match p.slot {
            j8framework::Slot::Data(v) => Some(v),
            j8framework::Slot::Accessor { .. } => None,
        })
    }

    /// 设 `o` 的私有成员 `key`（不经 JS 的检查：不可扩展的对象也能挂）
    pub fn set_private<'gc>(a: &Agent<'gc, '_>, o: JsObject<'gc>, key: JsSymbol<'gc>, v: Value<'gc>) {
        let mut d = o.borrow_mut(a.mc);
        d.privates.retain(|&(k, _)| !j8framework::Gc::ptr_eq(k, key));
        d.privates.push((key, Property::hidden(v)));
    }

    /// 删掉 `o` 的私有成员 `key`
    pub fn remove_private<'gc>(a: &Agent<'gc, '_>, o: JsObject<'gc>, key: JsSymbol<'gc>) {
        o.borrow_mut(a.mc).privates.retain(|&(k, _)| !j8framework::Gc::ptr_eq(k, key));
    }

    // ---- 终结回调 ----

    /// 一个随宿主对象回收而跑 `cb` 的终结器（`cb` 为空则什么也不跑）
    pub fn finalizer(&self, cb: Finalize, data: *mut c_void, hint: *mut c_void) -> Finalizer {
        Finalizer { queue: Arc::clone(&self.finalizers), pending: cb.map(|cb| Pending { cb, data, hint }) }
    }

    /// 排一个终结回调（`node_api_post_finalizer`）
    pub fn post(&self, p: Pending) {
        self.finalizers.push(p);
    }

    /// 跑掉排着的终结回调（嵌入方在事件循环的空档也可以调）
    pub fn run_finalizers(&self) {
        while let Some(p) = self.finalizers.pop() {
            // SAFETY: 插件给的终结回调，按 Node-API 的约定调
            unsafe { (p.cb)(self.as_ptr(), p.data, p.hint) };
        }
    }

    // ---- 实例数据、外部内存 ----

    pub fn set_instance(&self, data: *mut c_void, finalize: Finalize, hint: *mut c_void) {
        // 换掉旧的：旧的终结回调不跑（同 Node）
        self.instance.replace(Some(Instance { data, finalize: finalize.map(|cb| Pending { cb, data, hint }) }));
    }

    pub fn instance(&self) -> *mut c_void {
        self.instance.borrow().as_ref().map_or(core::ptr::null_mut(), |i| i.data)
    }

    /// 加一个清理钩子（同一对钩子与参数不能重复）
    pub fn add_cleanup(&self, hook: crate::CleanupHook, arg: *mut c_void) -> Result<(), Status> {
        let mut c = self.cleanup.borrow_mut();
        if c.iter().any(|&(h, a)| core::ptr::fn_addr_eq(h, hook) && a == arg.addr()) {
            return Err(Status::InvalidArg);
        }
        c.push((hook, arg.expose_provenance()));
        Ok(())
    }

    /// 去掉一个清理钩子
    pub fn remove_cleanup(&self, hook: crate::CleanupHook, arg: *mut c_void) -> Result<(), Status> {
        let mut c = self.cleanup.borrow_mut();
        let at = c.iter().position(|&(h, a)| core::ptr::fn_addr_eq(h, hook) && a == arg.addr()).ok_or(Status::InvalidArg)?;
        c.remove(at);
        Ok(())
    }

    pub fn node_version(&self) -> *const crate::NodeVersion {
        &raw const self.node_version
    }

    pub fn adjust_external_memory(&self, change: i64) -> i64 {
        let n = self.external_memory.get().saturating_add(change);
        self.external_memory.set(n);
        n
    }

    // ---- 打开插件 ----

    /// 在 `a` 的 context 里打开一个插件：建它的环境（随 global 对象一起活），调注册函数，给出它的 exports。
    ///
    /// # Errors
    /// 注册函数抛的异常
    pub fn open<'gc>(a: &mut Agent<'gc, '_>, register: crate::RegisterFn) -> Result<Value<'gc>, Error> {
        let key = |a: &Agent<'gc, '_>, d: &str| -> Global { a.global(Value::Symbol(a.mc.alloc(SymbolData { description: Some(Box::from(d)) }))) };
        let env = Box::new(Self {
            agent: Cell::new(None),
            handles: RefCell::default(),
            scopes: RefCell::default(),
            pending: RefCell::new(None),
            last: UnsafeCell::default(),
            wrap_key: key(a, "napi wrap"),
            tag_key: key(a, "napi type tag"),
            instance: RefCell::new(None),
            finalizers: Arc::default(),
            external_memory: Cell::new(0),
            cleanup: RefCell::default(),
            node_version: crate::NodeVersion::default(),
        });
        let ptr = NonNull::from(&*env);
        let holder = Self::host(a, Holder(env));
        let global = a.realm.global;
        let holder_key = a.mc.alloc(SymbolData { description: Some(Box::from("napi env")) });
        Self::set_private(a, global, holder_key, holder.into());
        // SAFETY: 刚放进宿主对象的 Box，随 global 对象一起活
        let env = unsafe { ptr.as_ref() };
        let exports: Value<'gc> = a.object().into();
        env.enter(a, |env| {
            let h = env.push(exports);
            // SAFETY: 插件的注册函数，按 Node-API 的约定调
            let r = unsafe { register(env.as_ptr(), h) };
            let v = env.value(r).unwrap_or(exports);
            env.run_finalizers();
            env.take_pending().map_or(Ok(v), Err)
        })
    }
}

impl Drop for Env {
    /// isolate 拆掉时：跑完排着的终结回调和实例数据的终结回调（不在 JS 里，要跑 JS 的 API 返回 `CannotRunJs`）
    fn drop(&mut self) {
        while let Some((hook, arg)) = self.cleanup.get_mut().pop() {
            // SAFETY: 插件给的清理钩子与参数
            unsafe { hook(core::ptr::with_exposed_provenance_mut(arg)) };
        }
        self.run_finalizers();
        if let Some(Instance { finalize: Some(p), .. }) = self.instance.get_mut().take() {
            // SAFETY: 插件给的终结回调
            unsafe { (p.cb)(self.as_ptr(), p.data, p.hint) };
        }
    }
}
