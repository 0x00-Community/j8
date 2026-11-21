use nodejs_semver::{Range, Version};

pub const LATEST: &str = "latest";

#[derive(Clone)]
pub enum Want {
    Latest,
    Tag(String),
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

#[derive(Clone)]
pub struct Spec {
    pub name: String,
    pub want: Want,
    pub exact: Option<Version>,
}

impl TryFrom<&str> for Spec {
    type Error = String;

    fn try_from(text: &str) -> Result<Self, String> {
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
    pub fn unscoped(name: &str) -> &str {
        name.rsplit('/').next().unwrap_or(name)
    }

    pub fn key(name: &str, version: &Version) -> String {
        format!("{}@{version}", name.replace('/', "+"))
    }
}

pub enum Dep {
    Npm(Spec),
    Other(String),
}

const ALIAS: &str = "npm:";

impl Dep {
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
