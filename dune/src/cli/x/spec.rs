use nodejs_semver::{Range, Version};

/// The dist-tag a package name without a version asks for
pub const LATEST: &str = "latest";

/// Which versions a request accepts
#[derive(Clone)]
pub enum Want {
    /// No version written (or `latest`): the `latest` dist-tag
    Latest,
    /// Another dist-tag (`latest`, `next` …)
    Tag(String),
    /// A semver range (an exact version is a range too)
    Range(Range),
}

impl From<&str> for Want {
    fn from(text: &str) -> Self {
        match text.trim() {
            "" => Self::Range(Range::any()),
            LATEST => Self::Latest,
            text => text.parse().map_or_else(|_| Self::Tag(text.to_owned()), Self::Range),
        }
    }
}

/// A package and the versions wanted: `name`, `name@^1`, `@scope/name@next`
#[derive(Clone)]
pub struct Spec {
    pub name: String,
    pub want: Want,
    /// The exact version written, which the cache can answer without the registry
    pub exact: Option<Version>,
}

impl TryFrom<&str> for Spec {
    type Error = String;

    fn try_from(text: &str) -> Result<Self, String> {
        // a scope's `@` comes first; the version's `@` is the one after it
        let at = text.char_indices().skip(1).find(|&(_, c)| c == '@').map(|(i, _)| i);
        let (name, version) = at.map_or((text, None), |i| (&text[..i], Some(&text[i + 1..])));
        let valid = !name.is_empty() && name.split('/').count() <= 2 && name.split('/').all(|s| !s.is_empty() && s != "." && s != "..");
        if !valid || (name.contains('/') && !name.starts_with('@')) {
            return Err(format!("{text}: not a package name"));
        }
        Ok(Self {
            name: name.to_owned(),
            want: version.map_or(Want::Latest, Want::from),
            exact: version.and_then(|v| v.parse().ok()),
        })
    }
}

impl Spec {
    /// The executable a package offers under its own name: the name without its scope
    pub fn unscoped(name: &str) -> &str {
        name.rsplit('/').next().unwrap_or(name)
    }

    /// The directory name standing for `name@version` in the cache (no `/`)
    pub fn key(name: &str, version: &Version) -> String {
        format!("{}@{version}", name.replace('/', "+"))
    }
}

/// A dependency's value in package.json
pub enum Dep {
    /// From the registry: `^1.2`, `latest`, or `npm:<name>@<range>` (an alias)
    Npm(Spec),
    /// git, URL, `file:`, `workspace:` and `user/repo`: not installed by dunx
    Other(String),
}

/// Prefix of an alias: the dependency is another package under this name
const ALIAS: &str = "npm:";

impl Dep {
    /// The dependency `name` with package.json value `value`
    pub fn new(name: &str, value: &str) -> Self {
        if let Some(target) = value.strip_prefix(ALIAS) {
            return Spec::try_from(target).map_or_else(|_| Self::Other(value.to_owned()), Self::Npm);
        }
        if value.contains(':') || value.contains('/') {
            return Self::Other(value.to_owned());
        }
        Self::Npm(Spec { name: name.to_owned(), want: Want::from(value), exact: None })
    }
}
