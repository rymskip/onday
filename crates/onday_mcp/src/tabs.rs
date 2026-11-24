//! The browser's tabs: those in its default context, and those in named
//! containers, each a context of its own with separate cookies and storage.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use onday::{Browser, BrowserContext, ContextOptions, Page};

/// A tab by the context it is in, a named container or the default, and its
/// place among that context's tabs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TabRef {
    pub(crate) container: Option<String>,
    pub(crate) index: usize,
}

/// One tab as listed.
pub(crate) struct Tab {
    pub(crate) at: TabRef,
    pub(crate) page: Page,
}

pub(crate) struct Tabs {
    default: BrowserContext,
    containers: BTreeMap<String, BrowserContext>,
    options: ContextOptions,
    pub(crate) current: TabRef,
}

impl Tabs {
    /// `options` is what a container is opened with, as the default context was.
    pub(crate) fn new(default: BrowserContext, options: ContextOptions) -> Self {
        Tabs {
            default,
            containers: BTreeMap::new(),
            options,
            current: TabRef {
                container: None,
                index: 0,
            },
        }
    }

    fn context(&self, container: Option<&str>) -> Result<&BrowserContext> {
        match container {
            None => Ok(&self.default),
            Some(name) => self
                .containers
                .get(name)
                .with_context(|| format!("no container {name:?}")),
        }
    }

    /// Every tab: the default context's first, then each container's by name.
    pub(crate) async fn list(&self) -> Result<Vec<Tab>> {
        let mut tabs = Vec::new();
        let contexts = std::iter::once((None, &self.default)).chain(
            self.containers
                .iter()
                .map(|(name, context)| (Some(name.clone()), context)),
        );
        for (container, context) in contexts {
            for (index, page) in context.pages().await?.into_iter().enumerate() {
                tabs.push(Tab {
                    at: TabRef {
                        container: container.clone(),
                        index,
                    },
                    page,
                });
            }
        }
        Ok(tabs)
    }

    /// The current tab. A closed one gives way to the last tab of its
    /// context, then to the default context's; `None` when that has none.
    pub(crate) async fn current_page(&mut self) -> Result<Option<Page>> {
        let pages = match self.context(self.current.container.as_deref()) {
            Ok(context) => context.pages().await?,
            Err(_) => Vec::new(),
        };
        if let Some(page) = pages.get(self.current.index) {
            return Ok(Some(page.clone()));
        }
        if let Some(last) = pages.last() {
            self.current.index = pages.len().saturating_sub(1);
            return Ok(Some(last.clone()));
        }
        let pages = self.default.pages().await?;
        let Some(last) = pages.last() else {
            self.current = TabRef {
                container: None,
                index: 0,
            };
            return Ok(None);
        };
        self.current = TabRef {
            container: None,
            index: pages.len().saturating_sub(1),
        };
        Ok(Some(last.clone()))
    }

    /// A new tab in `container`, or the default context, made current. A
    /// container is created the first time it is named.
    pub(crate) async fn new_page(
        &mut self,
        browser: &Browser,
        container: Option<&str>,
    ) -> Result<Page> {
        if let Some(name) = container
            && !self.containers.contains_key(name)
        {
            let context = browser
                .new_context(self.options.clone())
                .await
                .with_context(|| format!("create container {name:?}"))?;
            self.containers.insert(name.to_string(), context);
        }
        let context = self.context(container)?;
        let page = context.new_page().await?;
        self.current = TabRef {
            container: container.map(str::to_string),
            index: context.pages().await?.len().saturating_sub(1),
        };
        Ok(page)
    }

    /// The tab listed at `position`, made current.
    pub(crate) async fn select(&mut self, position: usize) -> Result<Page> {
        let tab = self.at(position).await?;
        self.current = tab.at;
        Ok(tab.page)
    }

    /// Close the tab listed at `position`, or the current one.
    pub(crate) async fn close(&mut self, position: Option<usize>) -> Result<()> {
        let tab = match position {
            Some(position) => self.at(position).await?.at,
            None => self.current.clone(),
        };
        let pages = self.context(tab.container.as_deref())?.pages().await?;
        let page = pages.get(tab.index).with_context(|| {
            format!(
                "no tab {} in {}",
                tab.index,
                label(tab.container.as_deref())
            )
        })?;
        page.close().await?;
        if self.current.container == tab.container
            && self.current.index >= tab.index
            && self.current.index > 0
        {
            self.current.index = self.current.index.saturating_sub(1);
        }
        Ok(())
    }

    async fn at(&self, position: usize) -> Result<Tab> {
        self.list()
            .await?
            .into_iter()
            .nth(position)
            .with_context(|| format!("no tab {position}"))
    }
}

/// How a listing names a tab's context.
pub(crate) fn label(container: Option<&str>) -> String {
    container.map_or_else(
        || "the default context".to_string(),
        |name| format!("container {name:?}"),
    )
}

/// One listing line: the marker for the current tab, its position, its
/// container when it has one, and its URL.
pub(crate) fn line(current: bool, position: usize, at: &TabRef, url: &str) -> String {
    let marker = if current { "*" } else { " " };
    match &at.container {
        Some(name) => format!("{marker} {position} [{name}]: {url}"),
        None => format!("{marker} {position}: {url}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{TabRef, label, line};

    #[test]
    fn a_tab_in_a_container_is_listed_with_its_name() {
        let at = TabRef {
            container: Some("karen".to_string()),
            index: 0,
        };
        assert_eq!(
            line(true, 2, &at, "https://a.test/"),
            "* 2 [karen]: https://a.test/"
        );
    }

    #[test]
    fn a_default_tab_is_listed_without_a_name() {
        let at = TabRef {
            container: None,
            index: 1,
        };
        assert_eq!(line(false, 1, &at, "about:blank"), "  1: about:blank");
    }

    #[test]
    fn an_error_names_the_context_it_looked_in() {
        assert_eq!(label(None), "the default context");
        assert_eq!(label(Some("karen")), "container \"karen\"");
    }
}
