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

#[derive(Clone, Copy, Debug)]
struct Scope {
    base: usize,
    escape: Option<(usize, bool)>,
}

#[derive(Clone, Copy, Debug)]
pub struct Pending {
    pub cb: unsafe extern "C" fn(env: *mut Env, data: *mut c_void, hint: *mut c_void),
    pub data: *mut c_void,
    pub hint: *mut c_void,
}

unsafe impl Send for Pending {}

unsafe impl Sync for Pending {}

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

#[derive(Debug)]
struct Instance {
    data: *mut c_void,
    finalize: Option<Pending>,
}

pub struct Env {
    agent: Cell<Option<NonNull<c_void>>>,
    handles: RefCell<Vec<u64>>,
    scopes: RefCell<Vec<Scope>>,
    pending: RefCell<Option<Error>>,
    last: UnsafeCell<crate::ExtendedErrorInfo>,
    wrap_key: Global,
    tag_key: Global,
    instance: RefCell<Option<Instance>>,
    finalizers: Arc<Fifo<Pending>>,
    external_memory: Cell<i64>,
    cleanup: RefCell<Vec<(crate::CleanupHook, usize)>>,
    node_version: crate::NodeVersion,
}

unsafe impl Send for Env {}

unsafe impl Sync for Env {}

struct Holder(#[expect(dead_code, reason = "only dropped along with the host object")] Box<Env>);

impl Env {
    pub fn agent<'a, 'scope>(&self) -> Result<&'a mut Agent<'scope, 'a>, Status> {
        let p = self.agent.get().ok_or(Status::CannotRunJs)?;
        Ok(unsafe { p.cast::<Agent<'scope, 'a>>().as_mut() })
    }

    pub fn enter<R>(&self, a: &mut Agent<'_, '_>, f: impl FnOnce(&Self) -> R) -> R {
        let outer = self.agent.replace(Some(NonNull::from(a).cast()));
        let (handles, scopes) = (self.handles.borrow().len(), self.scopes.borrow().len());
        let r = f(self);
        self.handles.borrow_mut().truncate(handles);
        self.scopes.borrow_mut().truncate(scopes);
        self.agent.set(outer);
        r
    }

    pub fn as_ptr(&self) -> *mut Self {
        core::ptr::from_ref(self).cast_mut()
    }

    pub fn push(&self, v: Value<'_>) -> Handle {
        let mut h = self.handles.borrow_mut();
        h.push(v.bits());
        Handle(NonZeroUsize::new(h.len()))
    }

    pub fn value<'scope>(&self, h: Handle) -> Result<Value<'scope>, Status> {
        let at = h.0.ok_or(Status::InvalidArg)?.get() - 1;
        let bits = *self.handles.borrow().get(at).ok_or(Status::InvalidArg)?;
        Ok(unsafe { Value::from_bits(bits) })
    }

    pub fn out(&self, out: *mut Handle, v: Value<'_>) -> Result<(), Status> {
        let h = self.push(v);
        crate::convert::Ffi::put(out, h)
    }

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

    pub fn close_scope(&self, scope: *mut c_void) -> Result<(), Status> {
        let mut s = self.scopes.borrow_mut();
        if scope.addr() != s.len() || s.is_empty() {
            return Err(Status::HandleScopeMismatch);
        }
        let top = s.pop().ok_or(Status::HandleScopeMismatch)?;
        self.handles.borrow_mut().truncate(top.base);
        Ok(())
    }

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

    pub fn done(&self, r: Result<(), Status>) -> Status {
        let status = r.err().unwrap_or(Status::Ok);
        let last = unsafe { &mut *self.last.get() };
        last.error_code = status;
        last.error_message = if status == Status::Ok { core::ptr::null() } else { status.message().as_ptr() };
        status
    }

    pub fn last_error(&self) -> *const crate::ExtendedErrorInfo {
        self.last.get()
    }

    pub fn preamble(&self) -> Result<(), Status> {
        if self.pending.borrow().is_some() { Err(Status::PendingException) } else { Ok(()) }
    }

    pub fn js<T>(&self, r: Result<T, Error>) -> Result<T, Status> {
        r.map_err(|e| {
            *self.pending.borrow_mut() = Some(e);
            Status::PendingException
        })
    }

    pub fn throw(&self, e: Error) {
        *self.pending.borrow_mut() = Some(e);
    }

    pub fn take_pending(&self) -> Option<Error> {
        self.pending.borrow_mut().take()
    }

    pub fn has_pending(&self) -> bool {
        self.pending.borrow().is_some()
    }

    pub fn wrap_key<'scope>(&self, a: &Agent<'scope, '_>) -> JsSymbol<'scope> {
        Self::symbol(a, &self.wrap_key)
    }

    pub fn tag_key<'scope>(&self, a: &Agent<'scope, '_>) -> JsSymbol<'scope> {
        Self::symbol(a, &self.tag_key)
    }

    fn symbol<'scope>(a: &Agent<'scope, '_>, g: &Global) -> JsSymbol<'scope> {
        match a.local(g).kind() {
            ValueKind::Symbol(s) => s,
            _ => unreachable!("the keys are symbols"),
        }
    }

    pub fn host<'scope>(a: &Agent<'scope, '_>, v: impl Any + Send + Sync) -> JsObject<'scope> {
        let data = a.mc.alloc(Static(AtomicRefCell::new(Box::new(v) as Box<dyn Any + Send + Sync>)));
        ObjectData { kind: ObjectKind::Host(data), proto: None, ..ObjectData::default() }.alloc(a.mc)
    }

    pub fn hosted<T: Any, R>(o: JsObject<'_>, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        let ObjectKind::Host(h) = o.borrow().kind else { return None };
        let mut cell = h.0.try_borrow_mut()?;
        cell.downcast_mut::<T>().map(f)
    }

    pub fn private<'scope>(o: JsObject<'scope>, key: JsSymbol<'scope>) -> Option<Value<'scope>> {
        o.borrow().privates.iter().find_map(|&(k, p)| (j8framework::Gc::ptr_eq(k, key)).then_some(p)).and_then(|p| match p.slot {
            j8framework::Slot::Data(v) => Some(v),
            j8framework::Slot::Accessor { .. } => None,
        })
    }

    pub fn set_private<'scope>(a: &Agent<'scope, '_>, o: JsObject<'scope>, key: JsSymbol<'scope>, v: Value<'scope>) {
        let mut d = o.borrow_mut(a.mc);
        d.privates.retain(|&(k, _)| !j8framework::Gc::ptr_eq(k, key));
        d.privates.push((key, Property::hidden(v)));
    }

    pub fn remove_private<'scope>(a: &Agent<'scope, '_>, o: JsObject<'scope>, key: JsSymbol<'scope>) {
        o.borrow_mut(a.mc).privates.retain(|&(k, _)| !j8framework::Gc::ptr_eq(k, key));
    }

    pub fn finalizer(&self, cb: Finalize, data: *mut c_void, hint: *mut c_void) -> Finalizer {
        Finalizer { queue: Arc::clone(&self.finalizers), pending: cb.map(|cb| Pending { cb, data, hint }) }
    }

    pub fn post(&self, p: Pending) {
        self.finalizers.push(p);
    }

    pub fn run_finalizers(&self) {
        while let Some(p) = self.finalizers.pop() {
            unsafe { (p.cb)(self.as_ptr(), p.data, p.hint) };
        }
    }

    pub fn set_instance(&self, data: *mut c_void, finalize: Finalize, hint: *mut c_void) {
        self.instance.replace(Some(Instance { data, finalize: finalize.map(|cb| Pending { cb, data, hint }) }));
    }

    pub fn instance(&self) -> *mut c_void {
        self.instance.borrow().as_ref().map_or(core::ptr::null_mut(), |i| i.data)
    }

    pub fn add_cleanup(&self, hook: crate::CleanupHook, arg: *mut c_void) -> Result<(), Status> {
        let mut c = self.cleanup.borrow_mut();
        if c.iter().any(|&(h, a)| core::ptr::fn_addr_eq(h, hook) && a == arg.addr()) {
            return Err(Status::InvalidArg);
        }
        c.push((hook, arg.expose_provenance()));
        Ok(())
    }

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

    pub fn open<'scope>(a: &mut Agent<'scope, '_>, register: crate::RegisterFn) -> Result<Value<'scope>, Error> {
        let key = |a: &Agent<'scope, '_>, d: &str| -> Global { a.global(Value::Symbol(a.mc.alloc(SymbolData { description: Some(Box::from(d)), private: false }))) };
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
        let holder_key = a.mc.alloc(SymbolData { description: Some(Box::from("napi env")), private: false });
        Self::set_private(a, global, holder_key, holder.into());
        let env = unsafe { ptr.as_ref() };
        let exports: Value<'scope> = a.object().into();
        env.enter(a, |env| {
            let h = env.push(exports);
            let r = unsafe { register(env.as_ptr(), h) };
            let v = env.value(r).unwrap_or(exports);
            env.run_finalizers();
            env.take_pending().map_or(Ok(v), Err)
        })
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        while let Some((hook, arg)) = self.cleanup.get_mut().pop() {
            unsafe { hook(core::ptr::with_exposed_provenance_mut(arg)) };
        }
        self.run_finalizers();
        if let Some(Instance { finalize: Some(p), .. }) = self.instance.get_mut().take() {
            unsafe { (p.cb)(self.as_ptr(), p.data, p.hint) };
        }
    }
}
