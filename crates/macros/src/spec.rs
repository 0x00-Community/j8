//! The shape of the JSON definitions. Unknown fields are rejected (`deny_unknown_fields`), so mistakes in the
//! JSON are never silently ignored.
use serde::Deserialize;

/// One JSON file: a number of enums and structs.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    /// Generate `Binary` (binary encoding) for every enum and struct, plus `pub const SCHEMA: u64` (a fingerprint of
    /// the JSON: binaries written under another definition are rejected). The caller provides `Binary`, `Reader` and
    /// `BinError`, and `Binary` for the field types not defined in the JSON
    #[serde(default)]
    pub binary: bool,
    #[serde(default)]
    pub enums: Vec<EnumSpec>,
    #[serde(default)]
    pub structs: Vec<StructSpec>,
}

/// One struct. Either all fields are named or none are (tuple struct).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructSpec {
    pub name: String,
    pub doc: String,
    /// Passed as-is into `#[derive(...)]`
    #[serde(default)]
    pub derive: Vec<String>,
    /// Associated constants
    #[serde(default)]
    pub consts: Vec<ConstSpec>,
    #[serde(default)]
    pub fields: Vec<StructField>,
}

/// One struct field (always `pub`).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructField {
    #[serde(default)]
    pub name: Option<String>,
    pub ty: String,
    #[serde(default)]
    pub doc: Option<String>,
}

/// `pub const name: ty = value;` (`value` is a Rust expression).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConstSpec {
    pub name: String,
    pub ty: String,
    pub value: String,
    #[serde(default)]
    pub doc: Option<String>,
}

/// One enum.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnumSpec {
    pub name: String,
    pub doc: String,
    /// Passed as-is into `#[derive(...)]`
    #[serde(default)]
    pub derive: Vec<String>,
    /// Integer type of the discriminant (`#[repr(..)]`): if omitted, the compiler chooses (it may use a niche in a
    /// field for the encoding, which costs a few extra instructions to read the discriminant)
    #[serde(default)]
    pub repr: Option<String>,
    /// Generate methods that return `&mut` to a field by its type
    #[serde(default)]
    pub accessors: Vec<Accessor>,
    pub variants: Vec<VariantSpec>,
}

/// `pub fn name(&mut self) -> Option<&mut ty>`: the first field of type `ty` in the variant.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Accessor {
    pub name: String,
    pub ty: String,
    pub doc: String,
}

/// One variant. Either all fields are named (struct variant) or none are (tuple variant).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariantSpec {
    pub name: String,
    #[serde(default)]
    pub doc: Option<String>,
    /// `#[default]`
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub fields: Vec<FieldSpec>,
}

/// One field.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldSpec {
    #[serde(default)]
    pub name: Option<String>,
    pub ty: String,
}
