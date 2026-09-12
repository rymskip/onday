//! The in-page runtime (`window.__onday`) and the wrappers that call into it.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::OnceLock;

/// JS body that scrolls `el` into view through its nearest user-scrollable
/// ancestor, then nudges it clear of sticky overlays. Expects `el` in scope.
pub const SCROLL_TO_TARGET_VIA_SCROLLBAR: &str = include_str!("js/scroll_to_target.js");

const LIB_TEMPLATE: &str = include_str!("js/lib.js");

struct Runtime {
    version: String,
    guard: String,
}

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        let mut hasher = DefaultHasher::new();
        LIB_TEMPLATE.hash(&mut hasher);
        SCROLL_TO_TARGET_VIA_SCROLLBAR.hash(&mut hasher);
        let version = format!("{:x}", hasher.finish());
        let lib = LIB_TEMPLATE
            .replace("/*__PRESCROLL__*/", SCROLL_TO_TARGET_VIA_SCROLLBAR)
            .replace("__ONDAY_VERSION__", &version);
        let guard =
            format!("if (!window.__onday || window.__onday.version !== '{version}') {{\n{lib}\n}}");
        Runtime { version, guard }
    })
}

/// Hash of the runtime source; pages running another version reinstall it.
pub fn runtime_version() -> &'static str {
    &runtime().version
}

/// Statement installing `window.__onday` unless this version is already present.
pub fn runtime_guard() -> &'static str {
    &runtime().guard
}

/// Function declaration calling `func(lib, ...args)` and returning its result as JSON text.
pub fn json_call(func: &str, prelude: &str) -> String {
    format!(
        "async function(...args) {{\n{guard}\n{prelude}\nconst __f = (\n{func}\n);\n\
         const __r = await __f(window.__onday, ...args);\n\
         return __ondayJson(__r);\n\
         function __ondayJson(v) {{ try {{ return JSON.stringify(v === undefined ? null : v); }} \
         catch (e) {{ return JSON.stringify(String(v)); }} }}\n}}",
        guard = runtime().guard,
    )
}

/// Function declaration calling `func(lib, ...args)` and returning an array of elements.
pub fn elements_call(func: &str, prelude: &str) -> String {
    format!(
        "async function(...args) {{\n{guard}\n{prelude}\nconst __f = (\n{func}\n);\n\
         const __r = await __f(window.__onday, ...args);\n\
         return Array.isArray(__r) ? __r : (__r ? [__r] : []);\n}}",
        guard = runtime().guard,
    )
}

/// Function declaration evaluating a user expression or function and returning JSON text.
///
/// A function value is called with the arguments; anything else is awaited as-is.
/// A trailing `;` is dropped so statement-style one-liners still parse.
pub fn user_call(expression: &str, prelude: &str) -> String {
    let expression = expression.trim().trim_end_matches(';');
    format!(
        "async function(...args) {{\n{prelude}\nconst __v = (\n{expression}\n);\n\
         const __r = typeof __v === 'function' ? await __v(...args) : await __v;\n\
         try {{ return JSON.stringify(__r === undefined ? null : __r); }} \
         catch (e) {{ return JSON.stringify(String(__r)); }}\n}}"
    )
}

/// A function declaration as a WebDriver Classic script body.
pub fn classic_body(declaration: &str) -> String {
    format!("return ({declaration}).apply(null, arguments);")
}

/// Run `scripts` once per document, keyed by a digest of their text.
pub fn init_prelude(scripts: &[String]) -> String {
    if scripts.is_empty() {
        return String::new();
    }
    let mut hasher = DefaultHasher::new();
    scripts.hash(&mut hasher);
    let key = format!("{:x}", hasher.finish());
    let body: String = scripts
        .iter()
        .map(|script| format!("try {{\n{script}\n}} catch (e) {{ console.error('onday init script failed', e); }}\n"))
        .collect();
    format!("if (window.__ondayInit !== '{key}') {{ window.__ondayInit = '{key}';\n{body}}}")
}

/// Wrap a script body as a BiDi preload function.
pub fn preload_function(script: &str) -> String {
    format!("() => {{\n{script}\n}}")
}

/// Script counting in-flight writes on `window.__ondayWritesInFlight`, using `predicate`.
pub fn writes_counter_script(predicate: &str) -> String {
    format!(
        "if (!window.__ondayWritesCounter) {{\n\
         window.__ondayWritesCounter = true;\n\
         window.__ondayWritesInFlight = 0;\n\
         const isWrite = ({predicate});\n\
         const originalFetch = window.fetch;\n\
         window.fetch = function(input, init) {{\n\
           const method = ((init && init.method) || (typeof input === 'object' && input.method) || 'GET').toUpperCase();\n\
           const url = typeof input === 'string' ? input : (input && input.url) || String(input);\n\
           const tracked = isWrite(method, url);\n\
           if (tracked) window.__ondayWritesInFlight++;\n\
           const settle = () => {{ if (tracked) window.__ondayWritesInFlight--; }};\n\
           return originalFetch.apply(window, arguments).then((r) => {{ settle(); return r; }}, (e) => {{ settle(); throw e; }});\n\
         }};\n\
         }}"
    )
}

/// Escape a string for a single-quoted JS literal.
pub fn js_single_quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            other => out.push(other),
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_substitutes_every_placeholder() {
        let guard = &runtime().guard;
        assert!(!guard.contains("__PRESCROLL__"));
        assert!(!guard.contains("__ONDAY_VERSION__"));
        assert!(guard.contains("findScrollable"));
    }

    #[test]
    fn user_expressions_lose_their_trailing_semicolon() {
        let call = user_call(
            "Object.defineProperty(navigator, 'webdriver', { get: () => false });\n",
            "",
        );
        assert!(call.contains("{ get: () => false })\n);"));
    }

    #[test]
    fn quoting_escapes_quotes_and_newlines() {
        assert_eq!(js_single_quoted("a'b\\c\nd"), "'a\\'b\\\\c\\nd'");
    }
}
