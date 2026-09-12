//! Tool parameter types.

use schemars::JsonSchema;
use serde::Deserialize;

/// Points at one element: a snapshot `ref`, or an onday `selector`.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct TargetParams {
    #[schemars(description = "Element ref from the latest browser_snapshot, e.g. \"e12\"")]
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
    #[schemars(
        description = "Selector instead of a ref: CSS, or text=…, role=button[name=\"Save\"], testid=…, label=…, joined with >>"
    )]
    #[serde(default)]
    pub selector: Option<String>,
    #[schemars(description = "Human-readable description of the element, for the log")]
    #[serde(default)]
    pub element: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum EngineParam {
    Chromium,
    Firefox,
    Webkit,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LaunchParams {
    #[schemars(
        description = "Engine to switch to: chromium, firefox or webkit. Closes the running browser first"
    )]
    #[serde(default)]
    pub engine: Option<EngineParam>,
    #[schemars(description = "Run without a window (default: the server's --headed setting)")]
    #[serde(default)]
    pub headless: Option<bool>,
    #[schemars(description = "Viewport width in CSS pixels")]
    #[serde(default)]
    pub width: Option<u32>,
    #[schemars(description = "Viewport height in CSS pixels")]
    #[serde(default)]
    pub height: Option<u32>,
    #[schemars(description = "Browser binary to launch instead of the configured one")]
    #[serde(default)]
    pub executable_path: Option<String>,
    #[schemars(description = "Force WebDriver Classic instead of BiDi")]
    #[serde(default)]
    pub classic: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NavigateParams {
    #[schemars(description = "URL to open")]
    pub url: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SnapshotParams {
    #[schemars(description = "Only snapshot the subtree under this ref")]
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ButtonParam {
    #[default]
    Left,
    Middle,
    Right,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClickParams {
    #[serde(flatten)]
    pub target: TargetParams,
    #[schemars(description = "Double-click instead of a single click")]
    #[serde(default)]
    pub double_click: bool,
    #[serde(default)]
    pub button: ButtonParam,
    #[schemars(description = "Keys held during the click, e.g. [\"Shift\"]")]
    #[serde(default)]
    pub modifiers: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TargetOnlyParams {
    #[serde(flatten)]
    pub target: TargetParams,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TypeParams {
    #[serde(flatten)]
    pub target: TargetParams,
    #[schemars(description = "Text to enter")]
    pub text: String,
    #[schemars(description = "Press Enter afterwards")]
    #[serde(default)]
    pub submit: bool,
    #[schemars(description = "Type key by key after the current value instead of replacing it")]
    #[serde(default)]
    pub slowly: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    Textbox,
    Checkbox,
    Radio,
    Combobox,
    Slider,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FormField {
    #[serde(flatten)]
    pub target: TargetParams,
    #[schemars(
        description = "Value: text, \"true\"/\"false\" for checkboxes and radios, the option label for comboboxes"
    )]
    pub value: String,
    #[schemars(description = "Field kind; textbox when omitted")]
    #[serde(default, rename = "type")]
    pub kind: Option<FieldKind>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FillFormParams {
    pub fields: Vec<FormField>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SelectParams {
    #[serde(flatten)]
    pub target: TargetParams,
    #[schemars(description = "Option values or labels to select")]
    pub values: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PressKeyParams {
    #[schemars(description = "Key or chord, e.g. \"Enter\", \"ArrowDown\", \"Control+A\"")]
    pub key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DragParams {
    #[schemars(description = "Ref of the element to drag")]
    #[serde(default)]
    pub start_ref: Option<String>,
    #[schemars(description = "Selector of the element to drag")]
    #[serde(default)]
    pub start_selector: Option<String>,
    #[schemars(description = "Ref of the drop target")]
    #[serde(default)]
    pub end_ref: Option<String>,
    #[schemars(description = "Selector of the drop target")]
    #[serde(default)]
    pub end_selector: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UploadParams {
    #[serde(flatten)]
    pub target: TargetParams,
    #[schemars(
        description = "Absolute paths of the files; the page's only <input type=file> when no target is given"
    )]
    pub paths: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DialogParams {
    #[schemars(description = "Accept (true) or dismiss (false) the dialog")]
    pub accept: bool,
    #[schemars(description = "Text for a prompt() dialog")]
    #[serde(default)]
    pub prompt_text: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitParams {
    #[schemars(description = "Wait until this text is visible")]
    #[serde(default)]
    pub text: Option<String>,
    #[schemars(description = "Wait until this text is gone")]
    #[serde(default)]
    pub text_gone: Option<String>,
    #[schemars(description = "Wait until an element matches this selector and is visible")]
    #[serde(default)]
    pub selector: Option<String>,
    #[schemars(description = "Just wait this many seconds")]
    #[serde(default)]
    pub time: Option<f64>,
    #[schemars(description = "Give up after this many seconds (default 30)")]
    #[serde(default)]
    pub timeout: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EvaluateParams {
    #[schemars(
        description = "JS expression, or a function; with a target it receives the element: (el) => el.textContent"
    )]
    pub function: String,
    #[serde(flatten)]
    pub target: TargetParams,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScreenshotParams {
    #[schemars(description = "Capture the whole scrollable page (BiDi sessions)")]
    #[serde(default)]
    pub full_page: bool,
    #[serde(flatten)]
    pub target: TargetParams,
    #[schemars(description = "File name under the session's screenshots/ directory")]
    #[serde(default)]
    pub filename: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConsoleParams {
    #[schemars(description = "Only this level or worse: debug, info, warn, error")]
    #[serde(default)]
    pub level: Option<String>,
    #[schemars(description = "Only messages containing this text")]
    #[serde(default)]
    pub filter: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NetworkParams {
    #[schemars(description = "Only URLs containing this text")]
    #[serde(default)]
    pub filter: Option<String>,
    #[schemars(description = "Include request and response headers")]
    #[serde(default)]
    pub headers: bool,
    #[schemars(description = "Only failed requests and 4xx/5xx responses")]
    #[serde(default)]
    pub failures_only: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RouteKind {
    Block,
    Fulfill,
    Continue,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RouteParams {
    #[schemars(
        description = "URL glob: ** spans anything, * stays within a path segment, e.g. \"**/api/users*\""
    )]
    pub pattern: String,
    pub action: RouteKind,
    #[schemars(description = "Status for fulfill (default 200)")]
    #[serde(default)]
    pub status: Option<u16>,
    #[schemars(description = "Body for fulfill")]
    #[serde(default)]
    pub body: Option<String>,
    #[schemars(description = "Content type for fulfill (default application/json)")]
    #[serde(default)]
    pub content_type: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UnrouteParams {
    pub pattern: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TabAction {
    List,
    New,
    Select,
    Close,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TabsParams {
    pub action: TabAction,
    #[schemars(description = "Tab index for select/close (default: the current tab for close)")]
    #[serde(default)]
    pub index: Option<usize>,
    #[schemars(description = "URL to open in a new tab")]
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ResizeParams {
    pub width: u32,
    pub height: u32,
}
