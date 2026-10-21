//! chromedriver speaks BiDi through a script it runs in a tab of the browser it
//! drives, the BiDi-CDP mapper. That tab also writes every message it sends into
//! a log on its own page, and laying the log out costs far more than the message:
//! a page that issues a thousand requests holds every command up for the seconds
//! the tab spends on it. With the log's element gone the tab finds nowhere to
//! write and skips the work.

use anyhow::{Context, Result};
use serde::Serialize;
use thirtyfour::WebDriver;
use thirtyfour::cdp::domains::runtime::Evaluate;
use thirtyfour::cdp::domains::target::{AttachToTarget, DetachFromTarget, GetTargets};
use thirtyfour::cdp::{CdpCommand, SessionId};

/// The title the mapper gives its tab.
const MAPPER_TITLE: &str = "BiDi-CDP Mapper";

/// The element the mapper looks up before each write to its log.
const REMOVE_LOG: &str = "document.getElementById('logs')?.remove()";

/// `Target.sendMessageToTarget`: the driver's command passthrough carries no
/// session id, so a command for another target travels inside this one.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SendMessageToTarget {
    session_id: SessionId,
    message: String,
}

#[derive(Serialize)]
struct Command<Params> {
    id: u32,
    method: &'static str,
    params: Params,
}

/// Remove the log from the mapper tab of `driver`'s session.
pub(crate) async fn silence_log(driver: &WebDriver) -> Result<()> {
    let cdp = driver.cdp();
    let targets = cdp
        .send(GetTargets)
        .await
        .context("list the browser's targets")?;
    // The tab is listed twice, once as the tab and once as the document in it.
    let mapper = targets
        .target_infos
        .into_iter()
        .find(|target| target.title == MAPPER_TITLE && target.r#type != "tab")
        .context("the browser lists no BiDi mapper tab")?;
    let session = cdp
        .send(AttachToTarget {
            target_id: mapper.target_id,
            flatten: false,
        })
        .await
        .context("attach to the BiDi mapper tab")?
        .session_id;
    let message = serde_json::to_string(&Command {
        id: 1,
        method: Evaluate::METHOD,
        params: Evaluate::new(REMOVE_LOG),
    })
    .context("encode the command for the BiDi mapper tab")?;
    let sent = cdp
        .send_raw(
            "Target.sendMessageToTarget",
            SendMessageToTarget {
                session_id: session.clone(),
                message,
            },
        )
        .await
        .context("remove the BiDi mapper tab's log");
    cdp.send(DetachFromTarget {
        session_id: Some(session),
    })
    .await
    .context("detach from the BiDi mapper tab")?;
    sent.map(drop)
}
