//! The shape of the JSON definitions. Unknown fields are rejected (`deny_unknown_fields`), so mistakes in the
//! JSON are never silently ignored.
use serde::Deserialize;

/// One JSON file: a number of enums.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub enums: Vec<EnumSpec>,
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
    /// Generate `Display`: mnemonic + operands
    #[serde(default)]
    pub display: bool,
    /// Generate assembly (the inverse of `Display`): `display` on ⇒ `FromStr`; all unit variants ⇒ `Operand` by
    /// variant name
    #[serde(default)]
    pub asm: bool,
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
    /// Mnemonic for `Display`; replaced by the `show: name` field if there is one
    #[serde(default)]
    pub mnemonic: Option<String>,
    /// Used with a `show: name` field: the mnemonic is `name.suffix` (another operand form of the same group of
    /// names, e.g. `add.smi`)
    #[serde(default)]
    pub suffix: Option<String>,
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
    /// Written before the value in `Display` (e.g. `fn#`, `extends `)
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub show: Show,
}

/// How a field appears in `Display`. An `Option<_>` field that is `None` is omitted entirely.
#[derive(Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum Show {
    /// `{}`
    #[default]
    Display,
    /// `{:?}`
    Debug,
    /// Replaces the mnemonic: `x.name()`
    Name,
}
