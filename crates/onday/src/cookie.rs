//! Cookies, as a context reads and sets them.

use serde::{Deserialize, Serialize};

/// A browser cookie. An unset field is left to the browser's default on add.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// Required on add over BiDi; over Classic the current page's domain.
    pub domain: Option<String>,
    pub path: Option<String>,
    /// Seconds since the Unix epoch; a session cookie when unset.
    pub expires: Option<i64>,
    pub http_only: Option<bool>,
    pub secure: Option<bool>,
    pub same_site: Option<SameSite>,
}

impl Cookie {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Cookie {
            name: name.into(),
            value: value.into(),
            domain: None,
            path: None,
            expires: None,
            http_only: None,
            secure: None,
            same_site: None,
        }
    }

    pub fn domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = Some(domain.into());
        self
    }

    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn expires(mut self, seconds_since_epoch: i64) -> Self {
        self.expires = Some(seconds_since_epoch);
        self
    }

    pub fn http_only(mut self, http_only: bool) -> Self {
        self.http_only = Some(http_only);
        self
    }

    pub fn secure(mut self, secure: bool) -> Self {
        self.secure = Some(secure);
        self
    }

    pub fn same_site(mut self, same_site: SameSite) -> Self {
        self.same_site = Some(same_site);
        self
    }
}

/// A cookie's `SameSite` attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SameSite {
    Strict,
    Lax,
    None,
}

impl SameSite {
    /// The attribute's wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            SameSite::Strict => "strict",
            SameSite::Lax => "lax",
            SameSite::None => "none",
        }
    }

    /// The attribute a browser reports; `default`, or anything unknown, is no attribute.
    pub(crate) fn from_wire(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "strict" => Some(SameSite::Strict),
            "lax" => Some(SameSite::Lax),
            "none" => Some(SameSite::None),
            _ => None,
        }
    }
}

impl From<thirtyfour::Cookie> for Cookie {
    fn from(cookie: thirtyfour::Cookie) -> Self {
        Cookie {
            name: cookie.name,
            value: cookie.value,
            domain: cookie.domain,
            path: cookie.path,
            expires: cookie.expiry,
            http_only: cookie.http_only,
            secure: cookie.secure,
            same_site: cookie.same_site.and_then(|same_site| match same_site {
                thirtyfour::SameSite::Strict => Some(SameSite::Strict),
                thirtyfour::SameSite::Lax => Some(SameSite::Lax),
                thirtyfour::SameSite::None => Some(SameSite::None),
                thirtyfour::SameSite::Default => None,
            }),
        }
    }
}

impl From<Cookie> for thirtyfour::Cookie {
    fn from(cookie: Cookie) -> Self {
        let mut out = thirtyfour::Cookie::new(cookie.name, cookie.value);
        out.domain = cookie.domain;
        out.path = cookie.path;
        out.expiry = cookie.expires;
        out.http_only = cookie.http_only;
        out.secure = cookie.secure;
        out.same_site = cookie.same_site.map(|same_site| match same_site {
            SameSite::Strict => thirtyfour::SameSite::Strict,
            SameSite::Lax => thirtyfour::SameSite::Lax,
            SameSite::None => thirtyfour::SameSite::None,
        });
        out
    }
}
