use crossterm::event::{KeyCode, KeyEvent};

use kiliax_core::config::Config;

use crate::input::InputLine;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelPickerFocus {
    Providers,
    Models,
}

#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub display: String,
    pub id: String,
}

#[derive(Debug, Clone)]
pub struct ProviderEntry {
    pub name: String,
    pub models: Vec<ModelEntry>,
}

#[derive(Debug, Clone)]
pub enum ModelPickerEvent {
    None,
    Cancel,
    Picked(String),
}

#[derive(Debug, Clone)]
pub struct ModelPicker {
    search: InputLine,
    focus: ModelPickerFocus,
    providers: Vec<ProviderEntry>,

    filtered_providers: Vec<usize>,
    provider_cursor: usize,

    filtered_models: Vec<usize>,
    model_cursor: usize,
}

impl ModelPicker {
    pub fn new(config: &Config, current_model_id: Option<&str>) -> Self {
        let mut providers: Vec<ProviderEntry> = Vec::new();
        for (name, provider) in &config.providers {
            let provider_prefix = format!("{name}/");
            let mut models = Vec::new();
            for raw in &provider.models {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let id = if trimmed.starts_with(&provider_prefix) {
                    trimmed.to_string()
                } else {
                    format!("{provider_prefix}{trimmed}")
                };
                models.push(ModelEntry {
                    display: trimmed.to_string(),
                    id,
                });
            }
            providers.push(ProviderEntry {
                name: name.to_string(),
                models,
            });
        }

        let (mut provider_cursor, mut model_cursor) = (0usize, 0usize);
        if let Some(current) = current_model_id.map(str::trim).filter(|s| !s.is_empty()) {
            if let Some((provider, _model)) = current.split_once('/') {
                if let Some(idx) = providers.iter().position(|p| p.name == provider) {
                    provider_cursor = idx;
                    if let Some(midx) = providers[idx]
                        .models
                        .iter()
                        .position(|m| m.id == current || m.display == current)
                    {
                        model_cursor = midx;
                    }
                }
            }
        }

        let mut picker = Self {
            search: InputLine::default(),
            focus: ModelPickerFocus::Providers,
            providers,
            filtered_providers: Vec::new(),
            provider_cursor: 0,
            filtered_models: Vec::new(),
            model_cursor: 0,
        };

        picker.filtered_providers = (0..picker.providers.len()).collect();
        if !picker.providers.is_empty() {
            picker.provider_cursor = provider_cursor.min(picker.providers.len().saturating_sub(1));
        }
        picker.refresh_filtered_models();
        picker.model_cursor = model_cursor.min(picker.filtered_models.len().saturating_sub(1));
        picker
    }

    pub fn focus(&self) -> ModelPickerFocus {
        self.focus
    }

    pub fn search_text(&self) -> &str {
        self.search.text()
    }

    pub fn search_cursor(&self) -> usize {
        self.search.cursor()
    }

    pub fn providers(&self) -> &[ProviderEntry] {
        &self.providers
    }

    pub fn filtered_providers(&self) -> &[usize] {
        &self.filtered_providers
    }

    pub fn provider_cursor(&self) -> usize {
        self.provider_cursor
    }

    pub fn filtered_models(&self) -> &[usize] {
        &self.filtered_models
    }

    pub fn model_cursor(&self) -> usize {
        self.model_cursor
    }

    pub fn selected_provider_index(&self) -> Option<usize> {
        self.filtered_providers.get(self.provider_cursor).copied()
    }

    pub fn selected_model(&self) -> Option<&ModelEntry> {
        let provider_idx = self.selected_provider_index()?;
        let model_idx = *self.filtered_models.get(self.model_cursor)?;
        self.providers.get(provider_idx)?.models.get(model_idx)
    }

    pub fn handle_paste(&mut self, text: &str) {
        self.search.insert_str(text);
        self.refresh_filters();
    }

    pub fn set_search_text(&mut self, text: impl Into<String>) {
        self.search.set_text(text);
        self.refresh_filters();
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ModelPickerEvent {
        match key.code {
            KeyCode::Esc => return ModelPickerEvent::Cancel,
            KeyCode::Tab | KeyCode::Left | KeyCode::Right => {
                self.toggle_focus();
                return ModelPickerEvent::None;
            }
            KeyCode::Up => {
                self.move_up();
                return ModelPickerEvent::None;
            }
            KeyCode::Down => {
                self.move_down();
                return ModelPickerEvent::None;
            }
            KeyCode::Enter => {
                if self.focus == ModelPickerFocus::Providers {
                    if !self.filtered_models.is_empty() {
                        self.focus = ModelPickerFocus::Models;
                    }
                    return ModelPickerEvent::None;
                }
                if let Some(model) = self.selected_model() {
                    return ModelPickerEvent::Picked(model.id.clone());
                }
                return ModelPickerEvent::None;
            }
            _ => {}
        }

        let before = self.search.text().to_string();
        let _ = self.search.handle_key(key);
        if before != self.search.text() {
            self.refresh_filters();
        }
        ModelPickerEvent::None
    }

    fn toggle_focus(&mut self) {
        if self.focus == ModelPickerFocus::Providers {
            if !self.filtered_models.is_empty() {
                self.focus = ModelPickerFocus::Models;
            }
        } else {
            self.focus = ModelPickerFocus::Providers;
        }
    }

    fn move_up(&mut self) {
        match self.focus {
            ModelPickerFocus::Providers => {
                if self.filtered_providers.is_empty() {
                    return;
                }
                if self.provider_cursor == 0 {
                    self.provider_cursor = self.filtered_providers.len().saturating_sub(1);
                } else {
                    self.provider_cursor = self.provider_cursor.saturating_sub(1);
                }
                self.model_cursor = 0;
                self.refresh_filtered_models();
            }
            ModelPickerFocus::Models => {
                if self.filtered_models.is_empty() {
                    return;
                }
                if self.model_cursor == 0 {
                    self.model_cursor = self.filtered_models.len().saturating_sub(1);
                } else {
                    self.model_cursor = self.model_cursor.saturating_sub(1);
                }
            }
        }
    }

    fn move_down(&mut self) {
        match self.focus {
            ModelPickerFocus::Providers => {
                if self.filtered_providers.is_empty() {
                    return;
                }
                self.provider_cursor = (self.provider_cursor + 1) % self.filtered_providers.len();
                self.model_cursor = 0;
                self.refresh_filtered_models();
            }
            ModelPickerFocus::Models => {
                if self.filtered_models.is_empty() {
                    return;
                }
                self.model_cursor = (self.model_cursor + 1) % self.filtered_models.len();
            }
        }
    }

    fn refresh_filters(&mut self) {
        let selected_provider = self
            .selected_provider_index()
            .and_then(|idx| self.providers.get(idx))
            .map(|p| p.name.clone());
        let selected_model_id = self.selected_model().map(|m| m.id.clone());

        let query = self.search.text().trim();
        self.filtered_providers.clear();
        for (idx, p) in self.providers.iter().enumerate() {
            if query.is_empty() {
                self.filtered_providers.push(idx);
                continue;
            }

            if fuzzy_match(query, &p.name).is_some() {
                self.filtered_providers.push(idx);
                continue;
            }

            if p.models.iter().any(|m| {
                fuzzy_match(query, &m.display).is_some() || fuzzy_match(query, &m.id).is_some()
            }) {
                self.filtered_providers.push(idx);
            }
        }

        if self.filtered_providers.is_empty() {
            self.provider_cursor = 0;
            self.filtered_models.clear();
            self.model_cursor = 0;
            self.focus = ModelPickerFocus::Providers;
            return;
        }

        if let Some(name) = selected_provider {
            if let Some(pos) = self
                .filtered_providers
                .iter()
                .position(|&idx| self.providers.get(idx).is_some_and(|p| p.name == name))
            {
                self.provider_cursor = pos;
            } else {
                self.provider_cursor = 0;
            }
        } else {
            self.provider_cursor = self
                .provider_cursor
                .min(self.filtered_providers.len().saturating_sub(1));
        }

        self.refresh_filtered_models();

        if let Some(id) = selected_model_id {
            if let Some(pos) = self.filtered_models.iter().position(|&idx| {
                self.selected_provider_models()
                    .get(idx)
                    .is_some_and(|m| m.id == id)
            }) {
                self.model_cursor = pos;
            } else {
                self.model_cursor = 0;
            }
        } else {
            self.model_cursor = self
                .model_cursor
                .min(self.filtered_models.len().saturating_sub(1));
        }

        if self.focus == ModelPickerFocus::Models && self.filtered_models.is_empty() {
            self.focus = ModelPickerFocus::Providers;
        }
    }

    fn refresh_filtered_models(&mut self) {
        let query = self.search.text().trim();
        let provider_models = self.selected_provider_models();

        let mut next_filtered = Vec::new();
        for (idx, m) in provider_models.iter().enumerate() {
            if query.is_empty()
                || fuzzy_match(query, &m.display).is_some()
                || fuzzy_match(query, &m.id).is_some()
            {
                next_filtered.push(idx);
            }
        }
        self.filtered_models = next_filtered;
        if self.model_cursor >= self.filtered_models.len() {
            self.model_cursor = 0;
        }
    }

    fn selected_provider_models(&self) -> &[ModelEntry] {
        let Some(idx) = self.selected_provider_index() else {
            return &[];
        };
        self.providers
            .get(idx)
            .map(|p| p.models.as_slice())
            .unwrap_or(&[])
    }
}

fn fuzzy_match(needle: &str, haystack: &str) -> Option<usize> {
    let needle = needle.trim();
    if needle.is_empty() {
        return Some(0);
    }

    let mut score = 0usize;
    let mut pos = 0usize;
    let needle = needle.to_ascii_lowercase();
    let hay = haystack.to_ascii_lowercase();

    for ch in needle.chars() {
        let rel = hay[pos..].find(ch)?;
        let idx = pos + rel;
        score = score.saturating_add(idx.saturating_sub(pos));
        pos = idx.saturating_add(ch.len_utf8());
    }

    Some(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_match_subsequence() {
        assert!(fuzzy_match("k2", "kimi-k2.5").is_some());
        assert!(fuzzy_match("zz", "kimi-k2.5").is_none());
    }

    #[test]
    fn model_picker_qualifies_models_with_slashes() {
        use std::collections::BTreeMap;

        use kiliax_core::config::{ProviderConfig, ProviderKind};

        let mut providers = BTreeMap::new();
        providers.insert(
            "openrouter".to_string(),
            ProviderConfig {
                kind: ProviderKind::OpenAICompatible,
                base_url: "https://openrouter.ai/api/v1/chat/completions".to_string(),
                api_key: None,
                models: vec!["openai/gpt-4o-mini".to_string()],
            },
        );

        let cfg = Config {
            default_model: None,
            providers,
            ..Default::default()
        };

        let picker = ModelPicker::new(&cfg, None);
        assert_eq!(picker.providers().len(), 1);
        assert_eq!(picker.providers()[0].name, "openrouter");
        assert_eq!(picker.providers()[0].models.len(), 1);
        assert_eq!(
            picker.providers()[0].models[0].display,
            "openai/gpt-4o-mini"
        );
        assert_eq!(
            picker.providers()[0].models[0].id,
            "openrouter/openai/gpt-4o-mini"
        );
    }
}
