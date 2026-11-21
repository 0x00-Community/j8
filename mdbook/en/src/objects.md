# Object model: shapes, stores, properties

## Object layout

```text
Object  + OBJECT_SHAPE     *const Shape (null: empty, or a dictionary)
        + OBJECT_INLINE    ValueCell × INLINE_PROPS (the first properties live in the object)
        + OBJECT_STORE     GC slot of the store tree: a leaf { capacity, ValueCell × capacity }
```

Objects keep what the hot paths touch outside their mutable data (`GcCell`): the prototype, the properties (a shape plus
a value store), array elements, and the immutable data of functions and regexps. The JIT reads and writes these fields
at the offsets `j8framework::Layout` gives.

j8's own heap structs are declared with `#[j8framework::heap]`; `#[mutable]` fields get the cell their type calls for
(`Value` ⇒ `ValueCell`, `Gc` ⇒ a paragc `Slot`, bools and integers ⇒ atomics) plus a getter and setter:

```rust,ignore
#[j8framework::heap]
pub struct Object<'gc> {
    #[mutable(set = write_proto)]
    proto: Option<JsObject<'gc>>,      // proto() / write_proto(mc, p)
    #[mutable]
    non_extensible: bool,              // non_extensible() / set_non_extensible(mc, b)
    data: GcCell<'gc, ObjectState<'gc>>,
}
```

## Shapes (hidden classes)

`shape.rs`: a shape is the **layout** of an object's own string-keyed properties, keys and attributes in insertion
order; the values are stored separately in the object's slots.

- Objects built the same way share shapes: adding a property follows the **transition table** to (or creates) a child
  shape;
- the execution layer keeps [inline caches](executor.md#inline-caches) keyed by "shape ⇒ slot index";
- shapes hold no `Gc` pointers (collection moves objects) and are managed with `Arc`; the transition table stores
  `Weak`, so shapes no object uses are freed naturally;
- deleting a property, changing attributes or having too many keys turns the object's properties into a
  **dictionary** (`Dict`, modified in place); inline caches don't accept those.

## The value store

`store.rs`: property values and array elements live in fixed-capacity leaves on the GC heap; large ones are a 128-ary
tree (300,000 elements in three levels). paragc only allocates fixed-size objects, so capacities come in classes:

- a leaf `Leaf<N>`: `N` `ValueCell`s (`N` = 2 / 4 / … / 128), the first word its capacity;
- an inner `Node`: 128 child slots, the first word `NODE` plus its level.

All `#[repr(C)]`: whatever `N`, the cells start after the head word, so they can be indexed with the type erased.
Reads don't borrow or lock; writes go through `ValueCell::set` and its write barrier.

## Internal methods

`property.rs` implements the spec's internal methods for ordinary objects (10.1) and arrays and string wrappers (10.4):

- `[[GetOwnProperty]]` / `[[DefineOwnProperty]]` (ValidateAndApplyPropertyDescriptor, ArraySetLength);
- `[[OwnPropertyKeys]]`, extensibility, the prototype;
- arrays have a **dense part** plus ordinary properties stored by index (holes, elements with different attributes);
  read-only `length` and non-extensibility follow the spec.

Property access (`ops.rs`) walks the prototype chain through these. Proxy (`proxy.rs`) dispatches the same entry points
to its traps and checks the invariants.

## Strings

`text.rs`: the representation shared by property keys and JS string values.

- **short strings** of at most 22 bytes are stored inline, without an allocation;
- long ones share one heap copy, and a piece cut out of one (`split`, `slice`, a regex match) points into it instead of
  copying (V8's sliced strings);
- when `+` produces something long enough, it is first a **rope** (two halves plus the total length), flattened and
  cached when the content is first needed, so a `s += c` loop doesn't copy the whole string every time; flattening and
  dropping are iterative, so ropes hundreds of thousands of levels deep don't overflow the stack;
- no `Gc` inside: the GC doesn't manage them.

Strings are stored as UTF-8 for now, so lone surrogates are lost; comparisons go by UTF-16 code units.

## Host objects

`ObjectKind::Host`: JS objects holding a Rust value (`HostData`, a `Box<dyn Any + Send + Sync>` behind a borrow flag).
Binding methods take it as `&self` / `&mut self`; a re-entrant `&mut self` call throws a `TypeError` instead of
panicking (see [Binding macros](bindings.md)).
