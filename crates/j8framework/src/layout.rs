use crate::{Closure, Env, Object, Props, ValueCell};

pub struct Layout;

impl Layout {
    pub const OBJECT_PROTO: usize = Object::PROTO_OFFSET;
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
    pub const CLOSURE_FUNC: usize = core::mem::offset_of!(Closure<'static>, func);
    pub const CLOSURE_ENV: usize = core::mem::offset_of!(Closure<'static>, env);
}
