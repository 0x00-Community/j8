//! Where machine code finds things: byte offsets into the heap objects a JIT reads and writes directly.
//!
//! ```text
//! Value                      NaN-boxed word (ValueBits); an object / context / string … is a pointer word: the top
//!                            16 bits ≥ 0xFFF8, the low 48 the data address
//! GC slot (proto, call …)    word: 0 empty, else a pointer word (low 48 bits the address)
//! ValueCell                  { word, counted }: the value word first
//! Object  + OBJECT_SHAPE     *const Shape (null: empty, or a dictionary)
//!         + OBJECT_INLINE    ValueCell × INLINE_PROPS (entries 0 … INLINE_PROPS − 1)
//!         + OBJECT_STORE     GC slot of the store: a leaf { head (its capacity, < 2^16), ValueCell × head }
//! Env     + ENV_SLOTS        tag byte (0 inline, 1 boxed), then inline: { len u8 at +8, ValueCell × 4 at +16 },
//!                            boxed: { *ValueCell, len } at +8
//! Fast    (Object + OBJECT_CALL ⇒ data)  tag byte (0 a closure), the closure at +FAST_CLOSURE
//! ```
use crate::{Closure, Env, Object, Props, ValueCell};

/// Byte offsets and sizes (see the module docs).
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
    /// A [`ValueCell`]'s size (its value word comes first)
    pub const CELL: usize = core::mem::size_of::<ValueCell<'static>>();
    /// A store leaf's cells start after its head word
    pub const STORE_CELLS: usize = core::mem::size_of::<usize>();
    /// Leaves hold at most this many cells (bigger stores are trees)
    pub const STORE_LEAF_MAX: usize = 128;
    pub const ENV_PARENT: usize = Env::PARENT_OFFSET;
    pub const ENV_TAG: usize = Env::SLOTS_OFFSET;
    pub const ENV_INLINE_TAG: u8 = 0;
    pub const ENV_INLINE_LEN: usize = Env::SLOTS_OFFSET + 8;
    pub const ENV_INLINE_CELLS: usize = Env::SLOTS_OFFSET + 16;
    pub const ENV_BOXED_PTR: usize = Env::SLOTS_OFFSET + 8;
    pub const ENV_INLINE_SLOTS: usize = 4;
    /// The closure tag of an object's [`Fast`](crate::Object) data
    pub const FAST_CLOSURE_TAG: u8 = 0;
    /// The closure inside the fast data
    pub const FAST_CLOSURE: usize = 8;
    pub const CLOSURE_FUNC: usize = core::mem::offset_of!(Closure<'static>, func);
    pub const CLOSURE_ENV: usize = core::mem::offset_of!(Closure<'static>, env);
}
