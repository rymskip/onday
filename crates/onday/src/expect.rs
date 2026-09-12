//! Retrying assertions: each polls until the condition holds or the timeout passes.

use std::future::Future;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use crate::locator::Locator;
use crate::page::Page;
use crate::poll::Backoff;

/// Default budget for assertions.
pub const DEFAULT_EXPECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Assertions about a locator.
pub fn expect(locator: &Locator) -> LocatorAssertions {
    LocatorAssertions {
        locator: locator.clone(),
        timeout: DEFAULT_EXPECT_TIMEOUT,
        negate: false,
    }
}

/// Assertions about a page.
pub fn expect_page(page: &Page) -> PageAssertions {
    PageAssertions {
        page: page.clone(),
        timeout: DEFAULT_EXPECT_TIMEOUT,
    }
}

async fn eventually<F, Fut>(
    timeout: Duration,
    negate: bool,
    what: String,
    mut probe: F,
) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = (bool, String)>,
{
    let start = Instant::now();
    let mut gap = Backoff::FAST.start;
    loop {
        let (held, observed) = probe().await;
        if held != negate {
            return Ok(());
        }
        if start.elapsed() > timeout {
            let expectation = if negate { format!("not {what}") } else { what };
            bail!(
                "expected {expectation}, but found {observed} (after {:?})",
                start.elapsed()
            );
        }
        tokio::time::sleep(gap).await;
        gap = Backoff::FAST.next_gap(gap);
    }
}

fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Debug, Clone)]
pub struct LocatorAssertions {
    locator: Locator,
    timeout: Duration,
    negate: bool,
}

impl LocatorAssertions {
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Invert the assertion.
    pub fn negated(mut self) -> Self {
        self.negate = !self.negate;
        self
    }

    fn what(&self, condition: &str) -> String {
        format!("{} {condition}", self.locator.selector())
    }

    pub async fn to_be_visible(&self) -> Result<()> {
        let locator = self.locator.clone();
        eventually(
            self.timeout,
            self.negate,
            self.what("to be visible"),
            || {
                let locator = locator.clone();
                async move {
                    match locator.is_visible().await {
                        Ok(visible) => (visible, format!("visible={visible}")),
                        Err(error) => (false, format!("{error:#}")),
                    }
                }
            },
        )
        .await
    }

    pub async fn to_be_hidden(&self) -> Result<()> {
        self.clone().negated().to_be_visible().await
    }

    pub async fn to_have_count(&self, expected: usize) -> Result<()> {
        let locator = self.locator.clone();
        eventually(
            self.timeout,
            self.negate,
            self.what(&format!("to match {expected} elements")),
            || {
                let locator = locator.clone();
                async move {
                    match locator.count().await {
                        Ok(count) => (count == expected, format!("{count}")),
                        Err(error) => (false, format!("{error:#}")),
                    }
                }
            },
        )
        .await
    }

    /// The element's whitespace-normalized text equals `expected`.
    pub async fn to_have_text(&self, expected: &str) -> Result<()> {
        let want = normalize(expected);
        self.text_check(format!("to have text {want:?}"), move |text| {
            normalize(text) == want
        })
        .await
    }

    /// The element's text contains `expected`.
    pub async fn to_contain_text(&self, expected: &str) -> Result<()> {
        let want = normalize(expected);
        self.text_check(format!("to contain text {want:?}"), move |text| {
            normalize(text).contains(&want)
        })
        .await
    }

    async fn text_check(&self, what: String, check: impl Fn(&str) -> bool) -> Result<()> {
        let locator = self.locator.clone();
        let check = &check;
        eventually(self.timeout, self.negate, self.what(&what), || {
            let locator = locator.first();
            async move {
                match locator.inner_text().await {
                    Ok(text) => (check(&text), format!("{text:?}")),
                    Err(error) => (false, format!("{error:#}")),
                }
            }
        })
        .await
    }

    pub async fn to_have_value(&self, expected: &str) -> Result<()> {
        let locator = self.locator.clone();
        let want = expected.to_string();
        eventually(
            self.timeout,
            self.negate,
            self.what(&format!("to have value {expected:?}")),
            || {
                let locator = locator.clone();
                let want = want.clone();
                async move {
                    match locator.input_value().await {
                        Ok(value) => (value == want, format!("{value:?}")),
                        Err(error) => (false, format!("{error:#}")),
                    }
                }
            },
        )
        .await
    }

    pub async fn to_be_enabled(&self) -> Result<()> {
        self.flag("to be enabled", |info| info.enabled).await
    }

    pub async fn to_be_disabled(&self) -> Result<()> {
        self.flag("to be disabled", |info| !info.enabled).await
    }

    pub async fn to_be_checked(&self) -> Result<()> {
        self.flag("to be checked", |info| info.checked == Some(true))
            .await
    }

    async fn flag(
        &self,
        what: &str,
        check: fn(&crate::locator::ElementInfo) -> bool,
    ) -> Result<()> {
        let locator = self.locator.clone();
        eventually(self.timeout, self.negate, self.what(what), || {
            let locator = locator.clone();
            async move {
                match locator.info().await {
                    Ok(info) => (check(&info), info.description),
                    Err(error) => (false, format!("{error:#}")),
                }
            }
        })
        .await
    }

    pub async fn to_have_attribute(&self, name: &str, expected: &str) -> Result<()> {
        let locator = self.locator.clone();
        let (name, want) = (name.to_string(), expected.to_string());
        eventually(
            self.timeout,
            self.negate,
            self.what(&format!("to have {name}={expected:?}")),
            || {
                let locator = locator.clone();
                let (name, want) = (name.clone(), want.clone());
                async move {
                    match locator.get_attribute(&name).await {
                        Ok(value) => (
                            value.as_deref() == Some(want.as_str()),
                            format!("{value:?}"),
                        ),
                        Err(error) => (false, format!("{error:#}")),
                    }
                }
            },
        )
        .await
    }
}

#[derive(Debug, Clone)]
pub struct PageAssertions {
    page: Page,
    timeout: Duration,
}

impl PageAssertions {
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The URL matches `glob` (see [`glob_matches`](crate::route::glob_matches)).
    pub async fn to_have_url(&self, glob: &str) -> Result<()> {
        let page = self.page.clone();
        let glob = glob.to_string();
        eventually(self.timeout, false, format!("URL matching {glob}"), || {
            let page = page.clone();
            let glob = glob.clone();
            async move {
                match page.url().await {
                    Ok(url) => (crate::route::glob_matches(&glob, &url), url),
                    Err(error) => (false, format!("{error:#}")),
                }
            }
        })
        .await
    }

    /// The URL contains `fragment`.
    pub async fn to_have_url_containing(&self, fragment: &str) -> Result<()> {
        let page = self.page.clone();
        let fragment = fragment.to_string();
        eventually(
            self.timeout,
            false,
            format!("URL containing {fragment}"),
            || {
                let page = page.clone();
                let fragment = fragment.clone();
                async move {
                    match page.url().await {
                        Ok(url) => (url.contains(&fragment), url),
                        Err(error) => (false, format!("{error:#}")),
                    }
                }
            },
        )
        .await
    }

    pub async fn to_have_title(&self, expected: &str) -> Result<()> {
        let page = self.page.clone();
        let want = expected.to_string();
        eventually(self.timeout, false, format!("title {expected:?}"), || {
            let page = page.clone();
            let want = want.clone();
            async move {
                match page.title().await {
                    Ok(title) => (title == want, format!("{title:?}")),
                    Err(error) => (false, format!("{error:#}")),
                }
            }
        })
        .await
    }
}
