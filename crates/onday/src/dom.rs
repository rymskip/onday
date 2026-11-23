//! Selector helpers.

/// Escape a string for a double-quoted HTML attribute value inside a CSS selector.
pub fn escape_attr(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// CSS selector for the test-id attribute named by the global [`AppHooks`](crate::AppHooks).
pub fn testid_selector(testid: &str) -> String {
    format!(
        r#"[{}="{}"]"#,
        crate::hooks::global().test_id_attribute(),
        escape_attr(testid)
    )
}
