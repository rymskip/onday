//! Aria snapshots: the runtime walks one document, and frames are spliced in under
//! their `<iframe>` entries before the whole tree renders as YAML.

use std::future::Future;
use std::pin::Pin;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::frame::Frame;
use crate::page::{ElementRef, JsArg, Page, Resolution};

/// Nodes walked per document before the snapshot is cut short.
const MAX_NODES: u32 = 5000;

#[derive(Debug, Deserialize)]
struct Walk {
    items: Vec<Node>,
    truncated: bool,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Node {
    Text { text: String },
    Element(Element),
}

#[derive(Debug, Deserialize)]
struct Element {
    role: String,
    name: String,
    #[serde(rename = "ref")]
    reference: String,
    states: Vec<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    url: Option<String>,
    /// A frame whose document the runtime can reach.
    #[serde(default)]
    frame: bool,
    children: Vec<Node>,
}

type Splice<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

impl Page {
    /// An accessibility-tree snapshot with `[ref=…]` labels usable in `ref=` selectors.
    /// Frames appear under their `iframe` entry, with refs like `f1e3`.
    pub async fn snapshot(&self) -> Result<String> {
        self.render_snapshot(&Frame::top(), None).await
    }

    /// Snapshot of `frame`'s document, or of `root` within it, as YAML.
    pub(crate) async fn render_snapshot(
        &self,
        frame: &Frame,
        root: Option<ElementRef>,
    ) -> Result<String> {
        let walk = self.walk(frame, root).await?;
        let mut lines = Vec::new();
        render(&walk.items, 0, &mut lines);
        if walk.truncated {
            lines.push(format!(
                "- text: \"… snapshot truncated at {MAX_NODES} nodes; snapshot a ref to see more\""
            ));
        }
        Ok(lines.join("\n"))
    }

    async fn walk(&self, frame: &Frame, root: Option<ElementRef>) -> Result<Walk> {
        let root = root.map_or(JsArg::Null, JsArg::El);
        let mut walk: Walk = self
            .call_json(
                frame,
                &format!("(lib, root, prefix) => lib.snapshot(root, {MAX_NODES}, prefix)"),
                vec![root, JsArg::Str(frame.prefix.clone())],
            )
            .await
            .context("take an aria snapshot")?;
        self.splice_frames(frame, &mut walk.items, &mut walk.truncated)
            .await?;
        Ok(walk)
    }

    /// Fill each reachable frame's entry with the frame's own tree.
    fn splice_frames<'a>(
        &'a self,
        frame: &'a Frame,
        nodes: &'a mut [Node],
        truncated: &'a mut bool,
    ) -> Splice<'a> {
        Box::pin(async move {
            for node in nodes {
                let Node::Element(element) = node else {
                    continue;
                };
                if !element.frame {
                    self.splice_frames(frame, &mut element.children, truncated)
                        .await?;
                    continue;
                }
                let selector = format!("ref={}", element.reference);
                let iframe = match self.resolve(frame, &selector).await? {
                    Resolution::Elements(mut found) if found.len() == 1 => found.remove(0),
                    other => bail!(
                        "frame {} did not resolve to itself: {other:?}",
                        element.reference
                    ),
                };
                let inner = self.content_frame(&iframe).await?;
                let walk = self
                    .walk(&inner, None)
                    .await
                    .with_context(|| format!("snapshot frame {}", element.reference))?;
                *truncated |= walk.truncated;
                element.children = walk.items;
            }
            Ok(())
        })
    }
}

fn quoted(text: &str) -> String {
    serde_json::Value::from(text).to_string()
}

fn render(nodes: &[Node], indent: usize, lines: &mut Vec<String>) {
    let pad = "  ".repeat(indent);
    for node in nodes {
        let element = match node {
            Node::Text { text } => {
                lines.push(format!("{pad}- text: {}", quoted(text)));
                continue;
            }
            Node::Element(element) => element,
        };
        let mut line = format!("{pad}- {}", element.role);
        if !element.name.is_empty() {
            line.push(' ');
            line.push_str(&quoted(&element.name));
        }
        for state in &element.states {
            line.push_str(&format!(" [{state}]"));
        }
        line.push_str(&format!(" [ref={}]", element.reference));
        let has_children = !element.children.is_empty();
        let value = element.value.as_deref().filter(|value| !value.is_empty());
        let url = element.url.as_deref();
        if let Some(value) = value
            && !has_children
            && url.is_none()
        {
            line.push_str(&format!(": {}", quoted(value)));
        }
        let nested_value = value.filter(|_| has_children);
        if has_children || url.is_some() || nested_value.is_some() {
            line.push(':');
        }
        lines.push(line);
        if let Some(url) = url {
            lines.push(format!("{pad}  - /url: {url}"));
        }
        if let Some(value) = nested_value {
            lines.push(format!("{pad}  - /value: {}", quoted(value)));
        }
        render(&element.children, indent + 1, lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_render_nested_under_their_iframe() {
        let walk: Walk = serde_json::from_str(
            r#"{"items":[
                {"role":"heading","name":"Title","ref":"e1","states":["level=1"],"children":[]},
                {"role":"link","name":"Home","ref":"e2","states":[],"url":"/","children":[]},
                {"role":"textbox","name":"Email","ref":"e3","states":[],"value":"a@b","children":[]},
                {"role":"iframe","name":"Canvas","ref":"e4","states":[],"frame":true,"children":[
                    {"role":"button","name":"Go","ref":"f1e1","states":[],"children":[]},
                    {"text":"said \"hi\""}
                ]}
            ],"truncated":false}"#,
        )
        .expect("decode");
        let mut lines = Vec::new();
        render(&walk.items, 0, &mut lines);
        assert_eq!(
            lines.join("\n"),
            [
                r#"- heading "Title" [level=1] [ref=e1]"#,
                r#"- link "Home" [ref=e2]:"#,
                r#"  - /url: /"#,
                r#"- textbox "Email" [ref=e3]: "a@b""#,
                r#"- iframe "Canvas" [ref=e4]:"#,
                r#"  - button "Go" [ref=f1e1]"#,
                r#"  - text: "said \"hi\"""#,
            ]
            .join("\n")
        );
    }
}
