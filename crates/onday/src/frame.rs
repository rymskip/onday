//! Documents inside a page: the top-level one and the frames nested in it, whatever
//! their origin.
//!
//! Element handles belong to the document that owns the element, so every page
//! script runs in the frame of the elements it touches.

use std::sync::Mutex;

use anyhow::{Result, bail};
use thirtyfour::WebElement;
use thirtyfour::bidi::BrowsingContextId;

use crate::page::ElementRef;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FrameTarget {
    Top,
    Bidi(BrowsingContextId),
    /// The iframe elements leading down from the top document, each valid in its parent.
    Classic(Vec<WebElement>),
}

#[derive(Debug, Clone)]
pub(crate) struct Frame {
    pub(crate) target: FrameTarget,
    /// Prefix of the refs this frame hands out: empty for the top document, else `f<n>`.
    pub(crate) prefix: String,
    /// The `<iframe>` or `<frame>` showing this document, in its parent frame. The
    /// parent measures and hit-tests it, which works across origins.
    pub(crate) host: Option<Box<ElementRef>>,
}

impl PartialEq for Frame {
    fn eq(&self, other: &Frame) -> bool {
        self.target == other.target
    }
}

impl Frame {
    pub(crate) fn top() -> Frame {
        Frame {
            target: FrameTarget::Top,
            prefix: String::new(),
            host: None,
        }
    }
}

/// Frames seen in a page, numbered in order so a frame keeps its ref prefix.
#[derive(Debug, Default)]
pub(crate) struct FrameRegistry {
    frames: Mutex<Vec<Frame>>,
}

impl FrameRegistry {
    /// The frame showing `target` inside `host`, numbered on first sight.
    pub(crate) fn register(&self, target: FrameTarget, host: ElementRef) -> Frame {
        let mut frames = self
            .frames
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let index = match frames.iter().position(|known| known.target == target) {
            Some(index) => index,
            None => {
                let prefix = format!("f{}", frames.len() + 1);
                frames.push(Frame {
                    target: target.clone(),
                    prefix,
                    host: None,
                });
                frames.len() - 1
            }
        };
        // The latest host wins: the element that showed the frame may have been replaced.
        frames[index].host = Some(Box::new(host));
        frames[index].clone()
    }

    pub(crate) fn by_prefix(&self, prefix: &str) -> Result<Frame> {
        if prefix.is_empty() {
            return Ok(Frame::top());
        }
        let frames = self
            .frames
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match frames.iter().find(|frame| frame.prefix == prefix) {
            Some(frame) => Ok(frame.clone()),
            None => bail!("no frame {prefix} in this page; take a fresh snapshot"),
        }
    }
}
