use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ffi::{c_char, c_void};

use j8framework::{Descriptor, Integrity, JsObject, PropertyKey, SymbolData, Value};

use crate::convert::Name;
use crate::convert::Ffi;
use crate::{Attributes, Callback, Env, Finalize, Finalizer, Handle, KeyConversion, KeyFilter, KeyMode, PropertyDescriptor, Status, TypeTag, napi};

struct Wrapped {
    data: *mut c_void,
    finalizer: Finalizer,
}

unsafe impl Send for Wrapped {}

unsafe impl Sync for Wrapped {}

pub struct External {
    data: *mut c_void,
    _finalizer: Finalizer,
}

unsafe impl Send for External {}

unsafe impl Sync for External {}

struct Attached(#[expect(dead_code, reason = "only dropped along with the host object")] Finalizer);

unsafe impl crate::Opaque<'_> for Wrapped {
    type Brand = Self;
}

unsafe impl crate::Opaque<'_> for External {
    type Brand = Self;
}

unsafe impl crate::Opaque<'_> for Attached {
    type Brand = Self;
}

unsafe impl crate::Opaque<'_> for TypeTag {
    type Brand = Self;
}

pub struct Reference<'e> {
    value: j8framework::ScopeGlobal<'e, j8framework::ValueBrand>,
    count: u32,
}

impl Env<'_> {
    pub fn define<'scope>(&self, a: &mut j8framework::Agent<'scope, '_>, target: JsObject<'scope>, p: &PropertyDescriptor) -> Result<(), Status> {
        let name: Name<'scope> = if p.utf8name.is_null() {
            self.name(p.name)?
        } else {
            Name::Str(a.string(&Ffi::utf8(p.utf8name, crate::AUTO_LENGTH)?))
        };
        let label = match &name {
            Name::Str(s) => alloc::string::String::from(&***s),
            Name::Sym(_) => alloc::string::String::new(),
        };
        let attrs = p.attributes;
        let func = |cb: Callback| cb.map(|cb| self.function(a, &label, cb, p.data, false));
        let desc = if p.getter.is_some() || p.setter.is_some() {
            Descriptor { get: Some(func(p.getter)), set: Some(func(p.setter)), enumerable: Some(attrs.has(Attributes::ENUMERABLE)), configurable: Some(attrs.has(Attributes::CONFIGURABLE)), ..Descriptor::default() }
        } else {
            let value = match p.method {
                Some(cb) => self.function(a, &label, cb, p.data, false).into(),
                None => self.value(p.value)?,
            };
            Descriptor {
                value: Some(value),
                writable: Some(attrs.has(Attributes::WRITABLE)),
                enumerable: Some(attrs.has(Attributes::ENUMERABLE)),
                configurable: Some(attrs.has(Attributes::CONFIGURABLE)),
                ..Descriptor::default()
            }
        };
        self.js(a.define_property_or_throw(target, name.key(), desc))
    }

    fn wrapped<'scope>(&self, a: &j8framework::Agent<'scope, '_>, o: JsObject<'scope>) -> Option<JsObject<'scope>> {
        Self::private(o, self.wrap_key(a)).and_then(Value::as_object)
    }

    fn keys<'scope>(&self, a: &mut j8framework::Agent<'scope, '_>, o: JsObject<'scope>, mode: KeyMode, filter: KeyFilter, conversion: KeyConversion) -> Result<Vec<Value<'scope>>, Status> {
        let mut out = Vec::new();
        let mut at = Some(o);
        while let Some(cur) = at {
            for k in self.js(a.own_property_keys(cur))? {
                let Some(p) = self.js(a.get_own_property(cur, Name::from(k).key()))? else { continue };
                let keep = (!filter.has(KeyFilter::ENUMERABLE) || p.enumerable)
                    && (!filter.has(KeyFilter::WRITABLE) || p.writable)
                    && (!filter.has(KeyFilter::CONFIGURABLE) || p.configurable);
                let v: Value<'scope> = match k {
                    PropertyKey::String(_) if filter.has(KeyFilter::SKIP_STRINGS) => continue,
                    PropertyKey::Symbol(_) if filter.has(KeyFilter::SKIP_SYMBOLS) => continue,
                    PropertyKey::String(s) => match (conversion, s.parse::<u32>()) {
                        (KeyConversion::KeepNumbers, Ok(i)) if i != u32::MAX && alloc::format!("{i}") == **s => f64::from(i).into(),
                        _ => s.into(),
                    },
                    PropertyKey::Symbol(s) => Value::Symbol(s),
                };
                if keep && !out.iter().any(|&x: &Value<'scope>| x.same_value(v)) {
                    out.push(v);
                }
            }
            at = match mode {
                KeyMode::OwnOnly => None,
                KeyMode::IncludePrototypes => self.js(a.get_prototype_of(cur))?,
            };
        }
        Ok(out)
    }
}

napi! {
    fn napi_create_object(env, result: *mut Handle) {
        let a = env.agent()?;
        env.out(result, a.object().into())
    }

    fn napi_create_array(env, result: *mut Handle) {
        let a = env.agent()?;
        env.out(result, a.array(Vec::new()).into())
    }

    fn napi_create_array_with_length(env, length: usize, result: *mut Handle) {
        let a = env.agent()?;
        let o = a.array(Vec::new());
        #[expect(clippy::cast_precision_loss, reason = "array lengths are at most 2^32 - 1")]
        let n = length.min(u32::MAX as usize) as f64;
        env.js(a.set_named(o.into(), "length", n.into()))?;
        env.out(result, o.into())
    }

    fn napi_get_array_length(env, value: Handle, result: *mut u32) {
        let a = env.agent()?;
        let v = env.value(value)?;
        if !env.js(a.is_array(v))? {
            return Err(Status::ArrayExpected);
        }
        let n = env.js(a.get_named(v, "length"))?;
        Ffi::put(result, a.to_uint32(n).map_err(|_| Status::ArrayExpected)?)
    }

    fn napi_get_prototype(env, object: Handle, result: *mut Handle) {
        env.preamble()?;
        let a = env.agent()?;
        let o = env.to_object(a, object)?;
        let p = env.js(a.get_prototype_of(o))?;
        env.out(result, p.map_or(Value::Null, Value::from))
    }

    fn napi_set_property(env, object: Handle, key: Handle, value: Handle) {
        env.preamble()?;
        let a = env.agent()?;
        let o = env.to_object(a, object)?;
        env.js(a.set_keyed(o.into(), env.value(key)?, env.value(value)?))
    }

    fn napi_get_property(env, object: Handle, key: Handle, result: *mut Handle) {
        env.preamble()?;
        let a = env.agent()?;
        let o = env.to_object(a, object)?;
        let v = env.js(a.get_keyed(o.into(), env.value(key)?))?;
        env.out(result, v)
    }

    fn napi_has_property(env, object: Handle, key: Handle, result: *mut bool) {
        env.preamble()?;
        let a = env.agent()?;
        let o = env.to_object(a, object)?;
        let b = env.js(a.has_keyed(o.into(), env.value(key)?))?;
        Ffi::put(result, b)
    }

    fn napi_delete_property(env, object: Handle, key: Handle, result: *mut bool) {
        env.preamble()?;
        let a = env.agent()?;
        let o = env.to_object(a, object)?;
        let b = env.js(a.delete_keyed(o.into(), env.value(key)?))?;
        Ffi::put_opt(result, b);
        Ok(())
    }

    fn napi_has_own_property(env, object: Handle, key: Handle, result: *mut bool) {
        env.preamble()?;
        let a = env.agent()?;
        let o = env.to_object(a, object)?;
        let name = env.name(key)?;
        let b = env.js(a.has_own_property(o, name.key()))?;
        Ffi::put(result, b)
    }

    fn napi_set_named_property(env, object: Handle, utf8name: *const c_char, value: Handle) {
        env.preamble()?;
        let a = env.agent()?;
        let o = env.to_object(a, object)?;
        let k = Ffi::utf8(utf8name, crate::AUTO_LENGTH)?;
        env.js(a.set_named(o.into(), &k, env.value(value)?))
    }

    fn napi_get_named_property(env, object: Handle, utf8name: *const c_char, result: *mut Handle) {
        env.preamble()?;
        let a = env.agent()?;
        let o = env.to_object(a, object)?;
        let k = Ffi::utf8(utf8name, crate::AUTO_LENGTH)?;
        let v = env.js(a.get_named(o.into(), &k))?;
        env.out(result, v)
    }

    fn napi_has_named_property(env, object: Handle, utf8name: *const c_char, result: *mut bool) {
        env.preamble()?;
        let a = env.agent()?;
        let o = env.to_object(a, object)?;
        let k = Ffi::utf8(utf8name, crate::AUTO_LENGTH)?;
        let b = env.js(a.has_property(o.into(), &k))?;
        Ffi::put(result, b)
    }

    fn napi_set_element(env, object: Handle, index: u32, value: Handle) {
        env.preamble()?;
        let a = env.agent()?;
        let o = env.to_object(a, object)?;
        env.js(a.set_index(o.into(), f64::from(index), env.value(value)?))
    }

    fn napi_get_element(env, object: Handle, index: u32, result: *mut Handle) {
        env.preamble()?;
        let a = env.agent()?;
        let o = env.to_object(a, object)?;
        let v = env.js(a.get_index(o.into(), f64::from(index)))?;
        env.out(result, v)
    }

    fn napi_has_element(env, object: Handle, index: u32, result: *mut bool) {
        env.preamble()?;
        let a = env.agent()?;
        let o = env.to_object(a, object)?;
        let b = env.js(a.has_index(o, f64::from(index)))?;
        Ffi::put(result, b)
    }

    fn napi_delete_element(env, object: Handle, index: u32, result: *mut bool) {
        env.preamble()?;
        let a = env.agent()?;
        let o = env.to_object(a, object)?;
        let b = env.js(a.delete_keyed(o.into(), f64::from(index).into()))?;
        Ffi::put_opt(result, b);
        Ok(())
    }

    fn napi_get_property_names(env, object: Handle, result: *mut Handle) {
        env.preamble()?;
        let a = env.agent()?;
        let o = env.to_object(a, object)?;
        let keys = env.keys(a, o, KeyMode::IncludePrototypes, KeyFilter(KeyFilter::ENUMERABLE | KeyFilter::SKIP_SYMBOLS), KeyConversion::NumbersToStrings)?;
        env.out(result, a.array(keys).into())
    }

    fn napi_get_all_property_names(env, object: Handle, key_mode: i32, key_filter: KeyFilter, key_conversion: i32, result: *mut Handle) {
        let (key_mode, key_conversion) = (KeyMode::try_from(key_mode)?, KeyConversion::try_from(key_conversion)?);
        env.preamble()?;
        let a = env.agent()?;
        let o = env.to_object(a, object)?;
        let keys = env.keys(a, o, key_mode, key_filter, key_conversion)?;
        env.out(result, a.array(keys).into())
    }

    fn napi_define_properties(env, object: Handle, property_count: usize, properties: *const PropertyDescriptor) {
        env.preamble()?;
        let a = env.agent()?;
        let o = env.object(object)?;
        for p in Ffi::slice(properties, property_count)? {
            env.define(a, o, p)?;
        }
        Ok(())
    }

    fn napi_object_freeze(env, object: Handle) {
        env.preamble()?;
        let a = env.agent()?;
        let o = env.object(object)?;
        env.js(a.set_integrity_level(o, Integrity::Frozen)).map(drop)
    }

    fn napi_object_seal(env, object: Handle) {
        env.preamble()?;
        let a = env.agent()?;
        let o = env.object(object)?;
        env.js(a.set_integrity_level(o, Integrity::Sealed)).map(drop)
    }

    fn napi_wrap(env, js_object: Handle, native_object: *mut c_void, finalize_cb: Finalize, finalize_hint: *mut c_void, result: *mut *mut Reference) {
        let a = env.agent()?;
        let o = env.object(js_object)?;
        if env.wrapped(a, o).is_some() {
            return Err(Status::InvalidArg);
        }
        let w = Env::host(a, Wrapped { data: native_object, finalizer: env.finalizer(finalize_cb, native_object, finalize_hint) });
        Env::set_private(a, o, env.wrap_key(a), w.into());
        Ffi::put_opt(result, Box::into_raw(Box::new(Reference { value: a.global(o.into()), count: 0 })));
        Ok(())
    }

    fn napi_unwrap(env, js_object: Handle, result: *mut *mut c_void) {
        let a = env.agent()?;
        let o = env.object(js_object)?;
        let w = env.wrapped(a, o).ok_or(Status::InvalidArg)?;
        Ffi::put(result, Env::hosted::<Wrapped, _>(a, w, |w| w.data).ok_or(Status::InvalidArg)?)
    }

    fn napi_remove_wrap(env, js_object: Handle, result: *mut *mut c_void) {
        let a = env.agent()?;
        let o = env.object(js_object)?;
        let w = env.wrapped(a, o).ok_or(Status::InvalidArg)?;
        let data = Env::hosted::<Wrapped, _>(a, w, |w| {
            w.finalizer.pending = None;
            w.data
        })
        .ok_or(Status::InvalidArg)?;
        Env::remove_private(a, o, env.wrap_key(a));
        Ffi::put_opt(result, data);
        Ok(())
    }

    fn napi_create_external(env, data: *mut c_void, finalize_cb: Finalize, finalize_hint: *mut c_void, result: *mut Handle) {
        let a = env.agent()?;
        let e = Env::host(a, External { data, _finalizer: env.finalizer(finalize_cb, data, finalize_hint) });
        env.out(result, e.into())
    }

    fn napi_get_value_external(env, value: Handle, result: *mut *mut c_void) {
        let a = env.agent()?;
        let o = env.value(value)?.as_object().ok_or(Status::InvalidArg)?;
        Ffi::put(result, Env::hosted::<External, _>(a, o, |e| e.data).ok_or(Status::InvalidArg)?)
    }

    fn napi_type_tag_object(env, value: Handle, type_tag: *const TypeTag) {
        let a = env.agent()?;
        let o = env.object(value)?;
        let key = env.tag_key(a);
        if Env::private(o, key).is_some() || type_tag.is_null() {
            return Err(Status::InvalidArg);
        }
        let tag = unsafe { type_tag.read() };
        Env::set_private(a, o, key, Env::host(a, tag).into());
        Ok(())
    }

    fn napi_check_object_type_tag(env, value: Handle, type_tag: *const TypeTag, result: *mut bool) {
        let a = env.agent()?;
        let o = env.object(value)?;
        if type_tag.is_null() {
            return Err(Status::InvalidArg);
        }
        let tag = unsafe { type_tag.read() };
        let same = Env::private(o, env.tag_key(a)).and_then(Value::as_object).and_then(|h| Env::hosted::<TypeTag, _>(a, h, |t| *t == tag));
        Ffi::put(result, same.unwrap_or(false))
    }

    fn napi_add_finalizer(env, js_object: Handle, finalize_data: *mut c_void, finalize_cb: Finalize, finalize_hint: *mut c_void, result: *mut *mut Reference) {
        let a = env.agent()?;
        let o = env.object(js_object)?;
        if finalize_cb.is_none() {
            return Err(Status::InvalidArg);
        }
        let key = a.mc.alloc(SymbolData { description: Some(Box::from("napi finalizer")), private: false });
        Env::set_private(a, o, key, Env::host(a, Attached(env.finalizer(finalize_cb, finalize_data, finalize_hint))).into());
        Ffi::put_opt(result, Box::into_raw(Box::new(Reference { value: a.global(o.into()), count: 0 })));
        Ok(())
    }

    fn node_api_post_finalizer(env, finalize_cb: Finalize, finalize_data: *mut c_void, finalize_hint: *mut c_void) {
        let cb = finalize_cb.ok_or(Status::InvalidArg)?;
        env.post(crate::Pending { cb, data: finalize_data, hint: finalize_hint });
        Ok(())
    }

    fn napi_create_reference(env, value: Handle, initial_refcount: u32, result: *mut *mut Reference) {
        let a = env.agent()?;
        let v = env.value(value)?;
        Ffi::put(result, Box::into_raw(Box::new(Reference { value: a.global(v), count: initial_refcount })))
    }

    fn napi_delete_reference(env, reference: *mut Reference) {
        let _ = env;
        if reference.is_null() {
            return Err(Status::InvalidArg);
        }
        drop(unsafe { Box::from_raw(reference) });
        Ok(())
    }

    fn napi_reference_ref(env, reference: *mut Reference, result: *mut u32) {
        let _ = env;
        let r = unsafe { reference.as_mut() }.ok_or(Status::InvalidArg)?;
        r.count += 1;
        Ffi::put_opt(result, r.count);
        Ok(())
    }

    fn napi_reference_unref(env, reference: *mut Reference, result: *mut u32) {
        let _ = env;
        let r = unsafe { reference.as_mut() }.ok_or(Status::InvalidArg)?;
        r.count = r.count.checked_sub(1).ok_or(Status::GenericFailure)?;
        Ffi::put_opt(result, r.count);
        Ok(())
    }

    fn napi_get_reference_value(env, reference: *mut Reference, result: *mut Handle) {
        let a = env.agent()?;
        let r = unsafe { reference.as_ref() }.ok_or(Status::InvalidArg)?;
        env.out(result, a.local(&r.value))
    }

    fn napi_set_instance_data(env, data: *mut c_void, finalize_cb: Finalize, finalize_hint: *mut c_void) {
        env.set_instance(data, finalize_cb, finalize_hint);
        Ok(())
    }

    fn napi_get_instance_data(env, data: *mut *mut c_void) {
        Ffi::put(data, env.instance())
    }

    fn napi_adjust_external_memory(env, change_in_bytes: i64, adjusted_value: *mut i64) {
        Ffi::put(adjusted_value, env.adjust_external_memory(change_in_bytes))
    }

    fn napi_get_version(env, result: *mut u32) {
        let _ = env;
        Ffi::put(result, crate::NAPI_VERSION)
    }
}
