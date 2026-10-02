//! Calling into the page runtime (`window.__onday`) over a Classic session.

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use thirtyfour::prelude::*;

use crate::js;

/// Call `func(lib, ...args)` in the page and decode its JSON result, installing the
/// runtime first when the document lacks it.
pub(super) async fn call_lib<T: DeserializeOwned>(
    driver: &WebDriver,
    func: &str,
    args: Vec<serde_json::Value>,
) -> Result<T> {
    let body = js::classic_body(&js::json_call(func, ""));
    let run = || async {
        driver
            .execute(body.clone(), args.clone())
            .await
            .context("run the runtime call")?
            .convert::<String>()
            .context("read the runtime call result")
    };
    let mut text = run().await?;
    if text == js::RUNTIME_MISSING {
        driver
            .execute(js::classic_body(&js::install_call()), Vec::new())
            .await
            .context("install the page runtime")?;
        text = run().await?;
    }
    serde_json::from_str(&text).with_context(|| format!("decode runtime call result {text}"))
}
