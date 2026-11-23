use crate::{Closure, Env, Object, Props, ValueCell};

pub struct Layout;

impl Layout {
    pub const OBJECT_PROTO: usize = Object::PROTO_OFFSET;
    pub const OBJECT_ELEMENTS: usize = Object::ELEMENTS_OFFSET;
    pub const OBJECT_FLAGS_WORD: usize = Object::SPARSE_OFFSET;
    pub const ARRAY_LEN: usize = Object::ELEMENTS_OFFSET + crate::ValueArray::LEN_OFFSET;
    pub const ARRAY_ROOT: usize = Object::ELEMENTS_OFFSET + crate::ValueArray::ROOT_OFFSET;
    pub const DENSE_ZERO_MASK: u64 = 0x00FF_FF00_FFFF_FFFF;
    pub const DENSE_ARRAY_BIT: u64 = 1 << 32;
    pub const NODE_MARK: usize = crate::Store::NODE_MARK;
    pub const NODE_TAIL_META: usize = crate::Store::TAIL_META;
    pub const NODE_TAIL: usize = crate::Store::TAIL_SLOT;
    pub const OBJECT_CALL: usize = Object::CALL_OFFSET;
    pub const OBJECT_CLASS: usize = Object::CLASS_OFFSET;
    pub const OBJECT_PLAIN_BIT: u8 = Object::PLAIN_BIT;
    pub const OBJECT_SHAPE: usize = Object::PROPS_OFFSET + Props::SHAPE_OFFSET;
    pub const OBJECT_STORE: usize = Object::PROPS_OFFSET + Props::STORE_OFFSET;
    pub const OBJECT_DICT: usize = Object::PROPS_OFFSET + Props::DICT_OFFSET;
    pub const OBJECT_INLINE: usize = Object::PROPS_OFFSET + Props::INLINE_OFFSET;
    pub const INLINE_PROPS: usize = Props::INLINE;
    pub const CELL: usize = core::mem::size_of::<ValueCell<'static>>();
    pub const STORE_CELLS: usize = core::mem::size_of::<usize>();
    pub const ENV_PARENT: usize = Env::PARENT_OFFSET;
    pub const ENV_TAG: usize = Env::SLOTS_OFFSET;
    pub const ENV_INLINE_TAG: u8 = 0;
    pub const ENV_INLINE_CELLS: usize = Env::SLOTS_OFFSET + 16;
    pub const ENV_BOXED_PTR: usize = Env::SLOTS_OFFSET + 8;
    pub const FAST_CLOSURE_TAG: u8 = 0;
    pub const FAST_CLOSURE: usize = 8;
    pub const FAST_NATIVE_TAG: u8 = Object::FAST_NATIVE_TAG;
    pub const FAST_NATIVE_FN: usize = Object::FAST_NATIVE_FN;
    pub const CLOSURE_FUNC: usize = core::mem::offset_of!(Closure<'static>, func);
    pub const CLOSURE_ENV: usize = core::mem::offset_of!(Closure<'static>, env);
    pub const STR_UNITS: usize = crate::JsStr::UNITS_OFFSET;
}

const _: () = assert!(core::mem::size_of::<crate::ValueCell<'static>>() == 16);
const _: () = assert!(Layout::STR_UNITS == crate::JsStr::UNITS_OFFSET);
const _: () = assert!(Layout::STR_UNITS == 12 && Layout::STR_UNITS + 4 <= 32);
const _: () = assert!(core::mem::size_of::<crate::JsStr<'static>>() == 32);
const _: () = assert!(core::mem::size_of::<crate::JsString<'static>>() == 8);
const _: () = assert!(Object::CLASS_OFFSET == Object::SPARSE_OFFSET + 4);
const _: () = assert!(Layout::ARRAY_LEN == 152 && Layout::ARRAY_ROOT == 136);
const _: () = assert!(Layout::NODE_TAIL_META == 8 + 128 * 16);
