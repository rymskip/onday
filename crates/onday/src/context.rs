//! Browser contexts: isolated cookie jars and storage, each holding pages.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use thirtyfour::bidi::UserContextId;
use thirtyfour::bidi::modules::browser::{
    DownloadBehavior, RemoveUserContext, SetDownloadBehavior,
};
use thirtyfour::bidi::modules::browsing_context::{CreateType, GetTree};
use thirtyfour::{Cookie, SameSite};

use crate::browser::{Browser, BrowserInner, Session};
use crate::events::DialogPolicy;
use crate::hooks::{self, AppHooks};
use crate::js;
use crate::page::{Page, PageInner, PageTarget};
use crate::proto::{
    AddPreloadScript, BytesValue, DeleteCookies, GetCookies, PartialCookie, Partition, SetCookie,
};

/// Options for a browser context.
#[derive(Clone)]
pub struct ContextOptions {
    /// Viewport applied to every page.
    pub viewport: Option<(u32, u32)>,
    /// App conventions; the global hooks when unset.
    pub hooks: Option<Arc<dyn AppHooks>>,
    /// What pages do with dialogs nobody handles.
    pub dialog_policy: DialogPolicy,
    /// Where downloads land (BiDi only).
    pub downloads_dir: Option<PathBuf>,
    /// Prefix for relative `goto` URLs.
    pub base_url: Option<String>,
}

impl Default for ContextOptions {
    fn default() -> Self {
        ContextOptions {
            viewport: None,
            hooks: None,
            dialog_policy: DialogPolicy::Dismiss,
            downloads_dir: None,
            base_url: None,
        }
    }
}

impl std::fmt::Debug for ContextOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextOptions")
            .field("viewport", &self.viewport)
            .field("dialog_policy", &self.dialog_policy)
            .field("downloads_dir", &self.downloads_dir)
            .field("base_url", &self.base_url)
            .finish()
    }
}

pub(crate) struct ContextInner {
    pub(crate) browser: Arc<BrowserInner>,
    pub(crate) session: Arc<Session>,
    pub(crate) user_context: Option<UserContextId>,
    pub(crate) hooks: Arc<dyn AppHooks>,
    pub(crate) options: ContextOptions,
    init_scripts: Mutex<Vec<String>>,
    pages: Mutex<Vec<Arc<PageInner>>>,
    owns_session: bool,
}

impl ContextInner {
    /// Scripts run at document start; Classic pages replay them before each command.
    pub(crate) fn init_scripts(&self) -> Vec<String> {
        self.init_scripts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn user_context_id(&self) -> UserContextId {
        self.user_context
            .clone()
            .unwrap_or_else(|| UserContextId::from("default".to_string()))
    }
}

/// An isolated set of pages sharing cookies and storage.
#[derive(Clone)]
pub struct BrowserContext {
    pub(crate) inner: Arc<ContextInner>,
}

impl std::fmt::Debug for BrowserContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserContext")
            .field("user_context", &self.inner.user_context)
            .field("options", &self.inner.options)
            .finish()
    }
}

impl BrowserContext {
    pub(crate) async fn open(
        browser: Arc<BrowserInner>,
        session: Arc<Session>,
        user_context: Option<UserContextId>,
        owns_session: bool,
        options: ContextOptions,
    ) -> Result<BrowserContext> {
        let hooks = options.hooks.clone().unwrap_or_else(hooks::global);
        let mut scripts: Vec<String> = hooks
            .init_scripts()
            .into_iter()
            .map(|script| script.into_owned())
            .collect();
        if let Some(predicate) = hooks.is_tracked_write_js() {
            scripts.push(js::writes_counter_script(predicate));
        }
        let context = BrowserContext {
            inner: Arc::new(ContextInner {
                browser,
                session,
                user_context,
                hooks,
                options,
                init_scripts: Mutex::new(Vec::new()),
                pages: Mutex::new(Vec::new()),
                owns_session,
            }),
        };
        for script in scripts {
            context.add_init_script(&script).await?;
        }
        if let (Some(bidi), Some(dir)) = (
            &context.inner.session.bidi,
            &context.inner.options.downloads_dir,
        ) {
            std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
            bidi.send(SetDownloadBehavior {
                download_behavior: Some(DownloadBehavior::Allowed {
                    destination_folder: dir.to_string_lossy().into_owned(),
                }),
                user_contexts: Some(vec![context.inner.user_context_id()]),
            })
            .await?;
        }
        Ok(context)
    }

    pub fn hooks(&self) -> &Arc<dyn AppHooks> {
        &self.inner.hooks
    }

    /// The browser this context belongs to.
    pub fn browser(&self) -> Browser {
        Browser {
            inner: self.inner.browser.clone(),
        }
    }

    /// Run `script` at the start of every document in this context from now on.
    pub async fn add_init_script(&self, script: &str) -> Result<()> {
        self.inner
            .init_scripts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(script.to_string());
        if let Some(bidi) = &self.inner.session.bidi {
            bidi.send(AddPreloadScript {
                function_declaration: js::preload_function(script),
                contexts: None,
                user_contexts: Some(vec![self.inner.user_context_id()]),
            })
            .await?;
        }
        Ok(())
    }

    /// Open a new tab.
    pub async fn new_page(&self) -> Result<Page> {
        let target = match &self.inner.session.bidi {
            Some(bidi) => {
                let created = bidi
                    .send(thirtyfour::bidi::modules::browsing_context::Create {
                        r#type: CreateType::Tab,
                        reference_context: None,
                        background: None,
                        user_context: self.inner.user_context.clone(),
                    })
                    .await?;
                PageTarget::Bidi(created.context)
            }
            None => {
                let mut current = self.inner.session.classic_window.lock().await;
                let handle = self
                    .inner
                    .session
                    .driver
                    .new_tab()
                    .await
                    .context("open a tab")?;
                self.inner
                    .session
                    .driver
                    .switch_to_window(handle.clone())
                    .await
                    .context("switch to the new tab")?;
                *current = Some(handle.clone());
                PageTarget::Classic(handle)
            }
        };
        self.adopt(target).await
    }

    /// Pages open in this context, including ones the page itself opened.
    pub async fn pages(&self) -> Result<Vec<Page>> {
        let live: Vec<PageTarget> = match &self.inner.session.bidi {
            Some(bidi) => {
                let tree = bidi
                    .send(GetTree {
                        max_depth: Some(0),
                        root: None,
                    })
                    .await?;
                let mine = self.inner.user_context_id();
                tree.contexts
                    .into_iter()
                    .filter(|info| {
                        info.user_context
                            .clone()
                            .unwrap_or_else(|| UserContextId::from("default".to_string()))
                            == mine
                    })
                    .map(|info| PageTarget::Bidi(info.context))
                    .collect()
            }
            None => self
                .inner
                .session
                .driver
                .windows()
                .await
                .context("list windows")?
                .into_iter()
                .map(PageTarget::Classic)
                .collect(),
        };
        let known = self.tracked();
        let mut pages = Vec::with_capacity(live.len());
        for target in live {
            match known.iter().find(|page| page.target == target) {
                Some(page) => pages.push(Page {
                    context: self.inner.clone(),
                    inner: page.clone(),
                }),
                None => pages.push(self.adopt(target).await?),
            }
        }
        *self
            .inner
            .pages
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            pages.iter().map(|page| page.inner.clone()).collect();
        Ok(pages)
    }

    /// The first open page, opening one if there is none.
    pub async fn page(&self) -> Result<Page> {
        match self.pages().await?.into_iter().next() {
            Some(page) => Ok(page),
            None => self.new_page().await,
        }
    }

    fn tracked(&self) -> Vec<Arc<PageInner>> {
        self.inner
            .pages
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    async fn adopt(&self, target: PageTarget) -> Result<Page> {
        let page = Page::attach(self.inner.clone(), target).await?;
        self.inner
            .pages
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(page.inner.clone());
        Ok(page)
    }

    /// Cookies visible to this context.
    pub async fn cookies(&self) -> Result<Vec<Cookie>> {
        match &self.inner.session.bidi {
            Some(bidi) => {
                let result = bidi
                    .send(GetCookies {
                        partition: self.partition(),
                    })
                    .await?;
                Ok(result
                    .cookies
                    .into_iter()
                    .map(|cookie| {
                        let mut out = Cookie::new(cookie.name, cookie.value.as_text());
                        out.domain = Some(cookie.domain);
                        out.path = Some(cookie.path);
                        out.secure = Some(cookie.secure);
                        out.http_only = Some(cookie.http_only);
                        out.expiry = cookie.expiry;
                        out.same_site = cookie.same_site.as_deref().and_then(|value| match value {
                            "strict" => Some(SameSite::Strict),
                            "lax" => Some(SameSite::Lax),
                            "none" => Some(SameSite::None),
                            "default" => Some(SameSite::Default),
                            _ => None,
                        });
                        out
                    })
                    .collect())
            }
            None => self
                .inner
                .session
                .driver
                .get_all_cookies()
                .await
                .context("read cookies"),
        }
    }

    /// Add cookies. On Classic the current page must already be on the cookie's domain.
    pub async fn add_cookies(&self, cookies: Vec<Cookie>) -> Result<()> {
        for cookie in cookies {
            match &self.inner.session.bidi {
                Some(bidi) => {
                    let domain = cookie
                        .domain
                        .clone()
                        .with_context(|| format!("cookie {} needs a domain", cookie.name))?;
                    bidi.send(SetCookie {
                        cookie: PartialCookie {
                            name: cookie.name.clone(),
                            value: BytesValue::String {
                                value: cookie.value.clone(),
                            },
                            domain,
                            path: cookie.path.clone(),
                            http_only: cookie.http_only,
                            secure: cookie.secure,
                            same_site: cookie
                                .same_site
                                .map(|same_site| format!("{same_site:?}").to_lowercase()),
                            expiry: cookie.expiry,
                        },
                        partition: self.partition(),
                    })
                    .await
                    .with_context(|| format!("set cookie {}", cookie.name))?;
                }
                None => self
                    .inner
                    .session
                    .driver
                    .add_cookie(cookie.clone())
                    .await
                    .with_context(|| format!("add cookie {}", cookie.name))?,
            }
        }
        Ok(())
    }

    pub async fn clear_cookies(&self) -> Result<()> {
        match &self.inner.session.bidi {
            Some(bidi) => {
                bidi.send(DeleteCookies {
                    partition: self.partition(),
                })
                .await?;
                Ok(())
            }
            None => self
                .inner
                .session
                .driver
                .delete_all_cookies()
                .await
                .context("delete cookies"),
        }
    }

    fn partition(&self) -> Partition {
        Partition::StorageKey {
            user_context: Some(self.inner.user_context_id()),
        }
    }

    /// Close every page; a BiDi user context or an owned session is removed too.
    pub async fn close(&self) -> Result<()> {
        if let (Some(bidi), Some(user_context)) =
            (&self.inner.session.bidi, &self.inner.user_context)
        {
            bidi.send(RemoveUserContext {
                user_context: user_context.clone(),
            })
            .await?;
            return Ok(());
        }
        if self.inner.owns_session {
            return self.inner.session.quit().await;
        }
        for page in self.pages().await? {
            page.close().await?;
        }
        Ok(())
    }
}
