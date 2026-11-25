//! W3C WebAuthn virtual-authenticator extension commands
//! (`/session/{id}/webauthn/authenticator`), so `navigator.credentials`
//! ceremonies can be answered by a software authenticator.

use anyhow::{Context, Result};
use http::Method;
use serde::Serialize;
use thirtyfour::common::command::FormatRequestData;
use thirtyfour::{RequestData, SessionId};

use crate::Page;

/// Options for [`Page::add_virtual_authenticator`]. The default models a platform
/// authenticator: CTAP2 over `internal`, resident keys, verification that succeeds.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VirtualAuthenticatorOptions {
    pub protocol: String,
    pub transport: String,
    pub has_resident_key: bool,
    pub has_user_verification: bool,
    pub is_user_verified: bool,
}

impl Default for VirtualAuthenticatorOptions {
    fn default() -> Self {
        Self {
            protocol: "ctap2".to_string(),
            transport: "internal".to_string(),
            has_resident_key: true,
            has_user_verification: true,
            is_user_verified: true,
        }
    }
}

#[derive(Debug)]
struct AddVirtualAuthenticator(serde_json::Value);

impl FormatRequestData for AddVirtualAuthenticator {
    fn format_request(&self, session_id: &SessionId) -> RequestData {
        RequestData::new(
            Method::POST,
            format!("session/{session_id}/webauthn/authenticator"),
        )
        .add_body(self.0.clone())
    }
}

#[derive(Debug)]
struct RemoveVirtualAuthenticator(String);

impl FormatRequestData for RemoveVirtualAuthenticator {
    fn format_request(&self, session_id: &SessionId) -> RequestData {
        RequestData::new(
            Method::DELETE,
            format!("session/{}/webauthn/authenticator/{}", session_id, self.0),
        )
    }
}

/// A virtual authenticator attached to a page's tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authenticator {
    id: String,
}

impl Authenticator {
    pub fn id(&self) -> &str {
        &self.id
    }
}

impl Page {
    /// Attach a virtual authenticator to this page's tab. It answers the
    /// ceremonies of this tab only, so attach it from the tab that runs them.
    pub async fn add_virtual_authenticator(
        &self,
        options: VirtualAuthenticatorOptions,
    ) -> Result<Authenticator> {
        let body = serde_json::to_value(&options).context("serialize authenticator options")?;
        let id = self
            .driver_on_tab()
            .await?
            .cmd(AddVirtualAuthenticator(body))
            .await
            .context("add virtual authenticator")?
            .value::<String>()
            .context("read add-virtual-authenticator response value")?;
        Ok(Authenticator { id })
    }

    /// Detach an authenticator, discarding its credentials.
    pub async fn remove_virtual_authenticator(&self, authenticator: &Authenticator) -> Result<()> {
        self.driver_on_tab()
            .await?
            .cmd(RemoveVirtualAuthenticator(authenticator.id.clone()))
            .await
            .context("remove virtual authenticator")?;
        Ok(())
    }
}
