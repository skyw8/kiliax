use anyhow::Result;

use std::collections::VecDeque;
use std::path::PathBuf;

use super::App;

#[derive(Debug, Clone)]
pub struct PendingImage {
    pub placeholder: String,
    pub source_path: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct QueuedSubmission {
    pub text: String,
    pub images: Vec<PendingImage>,
}

impl App {
    pub fn queued_len(&self) -> usize {
        self.queued_submissions.len()
    }

    pub(crate) fn queued_submissions(&self) -> &VecDeque<QueuedSubmission> {
        &self.queued_submissions
    }

    pub(crate) fn pop_next_queued_submission(&mut self) -> Option<QueuedSubmission> {
        self.queued_submissions.pop_front()
    }

    pub(crate) fn pop_last_queued_submission(&mut self) -> Option<QueuedSubmission> {
        self.queued_submissions.pop_back()
    }

    pub(super) fn clear_pending_images(&mut self) {
        self.pending_images.clear();
    }

    pub(super) fn attach_image(&mut self, path: PathBuf) -> Result<()> {
        let placeholder = format!("[img#{}]", self.next_image_placeholder_id);
        self.next_image_placeholder_id = self.next_image_placeholder_id.saturating_add(1);
        self.insert_image_placeholder(&placeholder);

        self.pending_images.push(PendingImage {
            placeholder,
            source_path: path,
        });

        Ok(())
    }

    fn insert_image_placeholder(&mut self, placeholder: &str) {
        let cursor = self.input.cursor();
        let text = self.input.text();
        let prev = if cursor > 0 {
            text.chars().nth(cursor.saturating_sub(1))
        } else {
            None
        };
        let next = text.chars().nth(cursor);

        if prev.is_some_and(|ch| !ch.is_whitespace()) {
            self.input.insert_str(" ");
        }

        self.input.insert_str(placeholder);

        let needs_trailing_space = match next {
            None => true,
            Some(ch) => !ch.is_whitespace(),
        };
        if needs_trailing_space {
            self.input.insert_str(" ");
        }
    }

    pub(super) fn strip_image_placeholders_from_text(&self, text: &str) -> String {
        if self.pending_images.is_empty() {
            return text.to_string();
        }
        let mut out = text.to_string();
        for img in &self.pending_images {
            if out.contains(img.placeholder.as_str()) {
                out = out.replace(img.placeholder.as_str(), "");
            }
        }
        out
    }

    pub(super) fn prune_pending_images_missing_placeholders(&mut self) {
        if self.pending_images.is_empty() {
            return;
        }
        let text = self.input.text();
        self.pending_images
            .retain(|img| text.contains(img.placeholder.as_str()));
    }

    pub(super) fn ensure_pending_image_placeholders(&mut self) {
        if self.pending_images.is_empty() {
            return;
        }

        let mut text = self.input.text().to_string();
        let mut changed = false;

        for img in &self.pending_images {
            if text.contains(img.placeholder.as_str()) {
                continue;
            }
            if text.chars().last().is_some_and(|ch| !ch.is_whitespace()) {
                text.push(' ');
            }
            text.push_str(img.placeholder.as_str());
            text.push(' ');
            changed = true;
        }

        if changed {
            self.input.set_text(text);
        }
    }

    pub(super) fn enqueue_submission(&mut self, text: String, images: Vec<PendingImage>) {
        let text = text.trim().to_string();
        if text.is_empty() && images.is_empty() {
            return;
        }

        if !text.is_empty() && super::parse_known_slash_command(&text).is_none() {
            self.prompt_history.push(text.clone());
        }
        self.reset_history_nav();
        self.queued_submissions
            .push_back(QueuedSubmission { text, images });
    }
}

