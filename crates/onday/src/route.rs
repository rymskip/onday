//! Request interception: match paused requests against URL globs and continue,
//! fail or answer them.

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use thirtyfour::bidi::modules::network::{ContinueRequest, FailRequest};
use thirtyfour::bidi::{BiDi, BrowsingContextId, InterceptId};

use crate::events::{Header, PageEvents};
use crate::proto::{BeforeRequestSentEvent, BytesValue, ProvideResponse, WireHeader};

/// A request paused by a route, as a handler sees it.
#[derive(Debug, Clone)]
pub struct InterceptedRequest {
    pub url: String,
    pub method: String,
    pub headers: Vec<Header>,
}

/// A handler's decision for one request.
#[derive(Debug, Clone)]
pub enum RouteAction {
    /// Send it to the network unchanged.
    Continue,
    /// Fail it as a network error.
    Abort,
    /// Answer it without touching the network.
    Fulfill(Fulfill),
}

/// A synthetic response.
#[derive(Debug, Clone)]
pub struct Fulfill {
    pub status: u16,
    pub headers: Vec<Header>,
    pub body: Vec<u8>,
}

impl Fulfill {
    pub fn new(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Fulfill {
            status,
            headers: vec![Header {
                name: "content-type".to_string(),
                value: content_type.to_string(),
            }],
            body: body.into(),
        }
    }

    pub fn json(status: u16, body: impl Into<String>) -> Self {
        Fulfill::new(status, "application/json", body.into())
    }

    pub fn text(status: u16, body: impl Into<String>) -> Self {
        Fulfill::new(status, "text/plain; charset=utf-8", body.into())
    }
}

/// Decides what happens to a request whose URL matched the route's glob.
pub type RouteHandler = Arc<dyn Fn(&InterceptedRequest) -> RouteAction + Send + Sync>;

struct Route {
    pattern: String,
    handler: RouteHandler,
}

#[derive(Default)]
struct RouteState {
    routes: Vec<Route>,
    intercept: Option<InterceptId>,
}

/// The routes installed on one page, newest first.
#[derive(Default)]
pub(crate) struct RouteTable {
    state: Mutex<RouteState>,
}

impl std::fmt::Debug for RouteTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.lock();
        f.debug_struct("RouteTable")
            .field(
                "patterns",
                &state
                    .routes
                    .iter()
                    .map(|route| route.pattern.as_str())
                    .collect::<Vec<_>>(),
            )
            .field("intercept", &state.intercept)
            .finish()
    }
}

impl RouteTable {
    fn lock(&self) -> std::sync::MutexGuard<'_, RouteState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Add a route; returns true when the page needs an intercept registered.
    pub(crate) fn add(&self, pattern: String, handler: RouteHandler) -> bool {
        let mut state = self.lock();
        state.routes.insert(0, Route { pattern, handler });
        state.intercept.is_none()
    }

    /// Remove routes with `pattern`; returns the intercept to release once none remain.
    pub(crate) fn remove(&self, pattern: &str) -> Option<InterceptId> {
        let mut state = self.lock();
        state.routes.retain(|route| route.pattern != pattern);
        if state.routes.is_empty() {
            state.intercept.take()
        } else {
            None
        }
    }

    pub(crate) fn set_intercept(&self, intercept: InterceptId) {
        self.lock().intercept = Some(intercept);
    }

    pub(crate) fn patterns(&self) -> Vec<String> {
        self.lock()
            .routes
            .iter()
            .map(|route| route.pattern.clone())
            .collect()
    }

    fn decide(&self, event: &BeforeRequestSentEvent) -> Option<RouteAction> {
        let state = self.lock();
        let ours = state
            .intercept
            .as_ref()
            .is_some_and(|intercept| event.intercepts.contains(intercept));
        if !ours {
            return None;
        }
        let request = InterceptedRequest {
            url: event.request.url.clone(),
            method: event.request.method.clone(),
            headers: event.request.headers.iter().map(Header::from).collect(),
        };
        let action = state
            .routes
            .iter()
            .find(|route| glob_matches(&route.pattern, &request.url))
            .map(|route| (route.handler)(&request))
            .unwrap_or(RouteAction::Continue);
        Some(action)
    }
}

pub(crate) fn dispatch(
    bidi: BiDi,
    context: BrowsingContextId,
    page: Arc<PageEvents>,
    event: BeforeRequestSentEvent,
) {
    let Some(action) = page.routes.decide(&event) else {
        return;
    };
    let request = event.request.request.clone();
    tokio::spawn(async move {
        let sent = match action {
            RouteAction::Continue => bidi
                .send(ContinueRequest {
                    request,
                    body: None,
                    cookies: None,
                    headers: None,
                    method: None,
                    url: None,
                })
                .await
                .map(|_| ()),
            RouteAction::Abort => bidi.send(FailRequest { request }).await.map(|_| ()),
            RouteAction::Fulfill(fulfill) => bidi
                .send(ProvideResponse {
                    request,
                    status_code: fulfill.status,
                    reason_phrase: None,
                    headers: fulfill
                        .headers
                        .iter()
                        .map(|header| WireHeader {
                            name: header.name.clone(),
                            value: BytesValue::String {
                                value: header.value.clone(),
                            },
                        })
                        .collect(),
                    body: BytesValue::Base64 {
                        value: base64::engine::general_purpose::STANDARD.encode(&fulfill.body),
                    },
                })
                .await
                .map(|_| ()),
        };
        if let Err(error) = sent {
            tracing::warn!("answering an intercepted request in {context:?} failed: {error}");
        }
    });
}

/// Match `url` against a glob: `**` spans anything, `*` anything but `/`,
/// `?` one character. A pattern without wildcards must equal the URL.
pub fn glob_matches(pattern: &str, url: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let url: Vec<char> = url.chars().collect();
    matches_from(&pattern, &url)
}

fn matches_from(pattern: &[char], text: &[char]) -> bool {
    match pattern.first() {
        None => text.is_empty(),
        Some('*') if pattern.get(1) == Some(&'*') => {
            let rest = &pattern[2..];
            (0..=text.len()).any(|skip| matches_from(rest, &text[skip..]))
        }
        Some('*') => {
            let rest = &pattern[1..];
            let limit = text.iter().position(|c| *c == '/').unwrap_or(text.len());
            (0..=limit).any(|skip| matches_from(rest, &text[skip..]))
        }
        Some('?') => !text.is_empty() && matches_from(&pattern[1..], &text[1..]),
        Some(c) => text.first() == Some(c) && matches_from(&pattern[1..], &text[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::glob_matches;

    #[test]
    fn double_star_spans_segments() {
        assert!(glob_matches(
            "**/api/*",
            "http://localhost:3000/v1/api/users"
        ));
        assert!(!glob_matches(
            "**/api/*",
            "http://localhost:3000/v1/api/users/7"
        ));
        assert!(glob_matches(
            "**/api/**",
            "http://localhost:3000/v1/api/users/7"
        ));
    }

    #[test]
    fn plain_patterns_are_exact() {
        assert!(glob_matches("https://example.com/", "https://example.com/"));
        assert!(!glob_matches(
            "https://example.com/",
            "https://example.com/x"
        ));
    }

    #[test]
    fn star_alone_matches_every_url() {
        assert!(glob_matches("**", "https://example.com/a/b?c=d"));
    }
}
