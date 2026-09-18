//! Typed WebDriver BiDi commands and events, for the shapes onday reads or builds
//! beyond thirtyfour's curated facades.

use serde::{Deserialize, Serialize};
use thirtyfour::bidi::modules::browsing_context::CaptureScreenshotResult;
use thirtyfour::bidi::modules::script::{AddPreloadScriptResult, Target};
use thirtyfour::bidi::{
    BidiCommand, BidiEvent, BrowsingContextId, Empty, InterceptId, RequestId, UserContextId,
};

// ── script ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum LocalValue {
    String {
        value: String,
    },
    Boolean {
        value: bool,
    },
    Number {
        value: i64,
    },
    Null,
    Array {
        value: Vec<LocalValue>,
    },
    #[serde(untagged)]
    Shared(SharedRef),
}

#[derive(Debug, Clone, Serialize)]
pub struct SharedRef {
    #[serde(rename = "sharedId")]
    pub shared_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallFunction {
    pub function_declaration: String,
    pub await_promise: bool,
    pub target: Target,
    pub arguments: Vec<LocalValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_activation: Option<bool>,
}

impl BidiCommand for CallFunction {
    const METHOD: &'static str = "script.callFunction";
    type Returns = CallResult;
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CallResult {
    Success {
        result: RemoteValue,
    },
    Exception {
        #[serde(rename = "exceptionDetails")]
        exception_details: ExceptionDetails,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExceptionDetails {
    pub text: String,
    #[serde(default)]
    pub line_number: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum RemoteValue {
    String {
        value: String,
    },
    Null,
    Undefined,
    Array {
        #[serde(default)]
        value: Vec<RemoteValue>,
    },
    Node {
        #[serde(rename = "sharedId", default)]
        shared_id: Option<String>,
    },
    Window {
        value: WindowProxy,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
pub struct WindowProxy {
    pub context: BrowsingContextId,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddPreloadScript {
    pub function_declaration: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contexts: Option<Vec<BrowsingContextId>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_contexts: Option<Vec<UserContextId>>,
}

impl BidiCommand for AddPreloadScript {
    const METHOD: &'static str = "script.addPreloadScript";
    type Returns = AddPreloadScriptResult;
}

// ── input ───────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct PerformActions {
    pub context: BrowsingContextId,
    pub actions: Vec<SourceActions>,
}

impl BidiCommand for PerformActions {
    const METHOD: &'static str = "input.performActions";
    type Returns = Empty;
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SourceActions {
    Pointer {
        id: String,
        parameters: PointerParameters,
        actions: Vec<PointerAction>,
    },
    Key {
        id: String,
        actions: Vec<KeyAction>,
    },
    Wheel {
        id: String,
        actions: Vec<WheelAction>,
    },
}

#[derive(Debug, Serialize)]
pub struct PointerParameters {
    #[serde(rename = "pointerType")]
    pub pointer_type: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PointerAction {
    PointerMove {
        x: i64,
        y: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration: Option<u64>,
        origin: &'static str,
    },
    PointerDown {
        button: u8,
    },
    PointerUp {
        button: u8,
    },
    Pause {
        duration: u64,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum KeyAction {
    KeyDown { value: String },
    KeyUp { value: String },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WheelAction {
    Scroll {
        x: i64,
        y: i64,
        #[serde(rename = "deltaX")]
        delta_x: i64,
        #[serde(rename = "deltaY")]
        delta_y: i64,
        origin: &'static str,
    },
}

// ── browsingContext ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct CaptureScreenshot {
    pub context: BrowsingContextId,
    pub origin: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip: Option<ScreenshotClip>,
}

impl BidiCommand for CaptureScreenshot {
    const METHOD: &'static str = "browsingContext.captureScreenshot";
    type Returns = CaptureScreenshotResult;
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ScreenshotClip {
    Element {
        element: SharedRef,
    },
    Box {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
}

// ── network ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum BytesValue {
    String { value: String },
    Base64 { value: String },
}

impl BytesValue {
    pub fn as_text(&self) -> String {
        match self {
            BytesValue::String { value } => value.clone(),
            BytesValue::Base64 { value } => {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD
                    .decode(value)
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .unwrap_or_else(|_| value.clone())
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WireHeader {
    pub name: String,
    pub value: BytesValue,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvideResponse {
    pub request: RequestId,
    pub status_code: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_phrase: Option<String>,
    pub headers: Vec<WireHeader>,
    pub body: BytesValue,
}

impl BidiCommand for ProvideResponse {
    const METHOD: &'static str = "network.provideResponse";
    type Returns = Empty;
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireRequest {
    pub request: RequestId,
    pub url: String,
    pub method: String,
    #[serde(default)]
    pub headers: Vec<WireHeader>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireResponse {
    pub status: u16,
    #[serde(default)]
    pub status_text: String,
    #[serde(default)]
    pub headers: Vec<WireHeader>,
    #[serde(default)]
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeRequestSentEvent {
    pub context: Option<BrowsingContextId>,
    pub request: WireRequest,
    pub timestamp: u64,
    #[serde(default)]
    pub is_blocked: bool,
    #[serde(default)]
    pub intercepts: Vec<InterceptId>,
}

impl BidiEvent for BeforeRequestSentEvent {
    const METHOD: &'static str = "network.beforeRequestSent";
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseCompletedEvent {
    pub context: Option<BrowsingContextId>,
    pub request: WireRequest,
    pub response: WireResponse,
    pub timestamp: u64,
}

impl BidiEvent for ResponseCompletedEvent {
    const METHOD: &'static str = "network.responseCompleted";
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchErrorEvent {
    pub context: Option<BrowsingContextId>,
    pub request: WireRequest,
    pub timestamp: u64,
    pub error_text: String,
}

impl BidiEvent for FetchErrorEvent {
    const METHOD: &'static str = "network.fetchError";
}

// ── log ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntryEvent {
    #[serde(rename = "type")]
    pub kind: String,
    pub level: String,
    #[serde(default)]
    pub text: Option<String>,
    pub timestamp: u64,
    pub source: LogSource,
    #[serde(default)]
    pub stack_trace: Option<StackTrace>,
}

impl BidiEvent for LogEntryEvent {
    const METHOD: &'static str = "log.entryAdded";
}

#[derive(Debug, Clone, Deserialize)]
pub struct LogSource {
    #[serde(default)]
    pub context: Option<BrowsingContextId>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackTrace {
    #[serde(default)]
    pub call_frames: Vec<CallFrame>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallFrame {
    pub url: String,
    pub line_number: i64,
    pub column_number: i64,
}

// ── storage ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Partition {
    StorageKey {
        #[serde(rename = "userContext", skip_serializing_if = "Option::is_none")]
        user_context: Option<UserContextId>,
    },
}

#[derive(Debug, Serialize)]
pub struct GetCookies {
    pub partition: Partition,
}

impl BidiCommand for GetCookies {
    const METHOD: &'static str = "storage.getCookies";
    type Returns = GetCookiesResult;
}

#[derive(Debug, Deserialize)]
pub struct GetCookiesResult {
    pub cookies: Vec<WireCookie>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireCookie {
    pub name: String,
    pub value: BytesValue,
    pub domain: String,
    pub path: String,
    #[serde(default)]
    pub http_only: bool,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub same_site: Option<String>,
    #[serde(default)]
    pub expiry: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct SetCookie {
    pub cookie: PartialCookie,
    pub partition: Partition,
}

impl BidiCommand for SetCookie {
    const METHOD: &'static str = "storage.setCookie";
    type Returns = PartitionResult;
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PartialCookie {
    pub name: String,
    pub value: BytesValue,
    pub domain: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secure: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiry: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct DeleteCookies {
    pub partition: Partition,
}

impl BidiCommand for DeleteCookies {
    const METHOD: &'static str = "storage.deleteCookies";
    type Returns = PartitionResult;
}

#[derive(Debug, Deserialize)]
pub struct PartitionResult {}
