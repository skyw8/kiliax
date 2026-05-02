use async_openai::config::Config as OpenAIConfigTrait;
use reqwest::header::{HeaderMap, AUTHORIZATION};
use secrecy::{ExposeSecret, SecretString};

#[derive(Debug, Clone)]
pub(super) struct KiliaxOpenAIConfig {
    api_base: String,
    api_key: SecretString,
    send_auth: bool,
}

impl KiliaxOpenAIConfig {
    pub(super) fn new(api_base: &str, api_key: Option<&str>) -> Self {
        let api_base = normalize_api_base(api_base);
        let send_auth = api_key.is_some();
        let api_key = SecretString::from(api_key.unwrap_or_default().to_string());
        Self {
            api_base,
            api_key,
            send_auth,
        }
    }
}

impl OpenAIConfigTrait for KiliaxOpenAIConfig {
    fn headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if self.send_auth {
            headers.insert(
                AUTHORIZATION,
                format!("Bearer {}", self.api_key.expose_secret())
                    .as_str()
                    .parse()
                    .unwrap(),
            );
        }
        headers
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.api_base, path)
    }

    fn query(&self) -> Vec<(&str, &str)> {
        vec![]
    }

    fn api_base(&self) -> &str {
        &self.api_base
    }

    fn api_key(&self) -> &SecretString {
        &self.api_key
    }
}

fn normalize_api_base(api_base: &str) -> String {
    api_base.trim().trim_end_matches('/').to_string()
}
