//! JSON 定义的形状。未知字段直接报错（`deny_unknown_fields`），JSON 写错不会被悄悄忽略。
use serde::Deserialize;

/// 一个 JSON 文件：若干枚举。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub enums: Vec<EnumSpec>,
}

/// 一个枚举。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnumSpec {
    pub name: String,
    pub doc: String,
    /// 原样进 `#[derive(...)]`
    #[serde(default)]
    pub derive: Vec<String>,
    /// 生成 `Display`：助记符 + 操作数
    #[serde(default)]
    pub display: bool,
    /// 生成按字段类型取 `&mut` 的方法
    #[serde(default)]
    pub accessors: Vec<Accessor>,
    pub variants: Vec<VariantSpec>,
}

/// `pub fn name(&mut self) -> Option<&mut ty>`：变体里第一个类型为 `ty` 的字段。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Accessor {
    pub name: String,
    pub ty: String,
    pub doc: String,
}

/// 一个变体。字段要么全有名字（结构体变体），要么全没有（元组变体）。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariantSpec {
    pub name: String,
    #[serde(default)]
    pub doc: Option<String>,
    /// `Display` 的助记符；有 `show: name` 的字段时由它代替
    #[serde(default)]
    pub mnemonic: Option<String>,
    /// `#[default]`
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub fields: Vec<FieldSpec>,
}

/// 一个字段。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldSpec {
    #[serde(default)]
    pub name: Option<String>,
    pub ty: String,
    /// `Display` 时写在值前面（如 `fn#`、`extends `）
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub show: Show,
}

/// 字段在 `Display` 里怎么出现。`Option<_>` 字段为 `None` 时整个不出现。
#[derive(Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum Show {
    /// `{}`
    #[default]
    Display,
    /// `{:?}`
    Debug,
    /// 代替助记符：`x.name()`
    Name,
}
