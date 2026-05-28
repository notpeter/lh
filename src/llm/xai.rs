use serde_json::{Value, json};

use crate::common::LhResult;
use crate::config::LlmConfig;

use super::{LlmProvider, api_error, curl_post_json, model_for, thread_content_for_request};

const XAI_CHAT_COMPLETIONS_URL: &str = "https://api.x.ai/v1/chat/completions";

pub struct XaiProvider;

impl LlmProvider for XaiProvider {
    fn name(&self) -> &'static str {
        "xAI"
    }

    fn default_model(&self) -> &'static str {
        "grok-latest"
    }

    fn generate_title(&self, config: &LlmConfig, thread_content: &str) -> LhResult<String> {
        let api_key = std::env::var("XAI_API_KEY")
            .map_err(|_| "XAI_API_KEY must be set for provider = \"xai\"")?;
        let body = json!({
            "model": model_for(config, self),
            "stream": false,
            "temperature": 0.2,
            "max_tokens": 128,
            "messages": [
                {
                    "role": "system",
                    "content": config.prompt
                },
                {
                    "role": "user",
                    "content": thread_content_for_request(thread_content)
                }
            ]
        })
        .to_string();

        let response = curl_post_json(
            XAI_CHAT_COMPLETIONS_URL,
            &[format!("Authorization: Bearer {api_key}")],
            body,
            self.name(),
        )?;
        let value = serde_json::from_str::<Value>(&response)?;
        if let Some(error) = api_error(&value, self.name()) {
            return Err(error.into());
        }
        value
            .get("choices")
            .and_then(|value| value.as_array())
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(|content| content.as_str())
            .map(ToString::to_string)
            .ok_or_else(|| "xAI response did not include choices[0].message.content".into())
    }
}
