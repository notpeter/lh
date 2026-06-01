use serde_json::{Value, json};

use crate::common::LhResult;
use crate::config::LlmConfig;

use super::{
    LlmProvider, TitleResponse, api_error, curl_post_json, model_for, thread_content_for_request,
};

const ANTHROPIC_MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicProvider;

impl LlmProvider for AnthropicProvider {
    fn id(&self) -> &'static str {
        "anthropic"
    }

    fn name(&self) -> &'static str {
        "Anthropic"
    }

    fn default_model(&self) -> &'static str {
        "claude-haiku-4-5"
    }

    fn generate_title(&self, config: &LlmConfig, thread_content: &str) -> LhResult<TitleResponse> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| "ANTHROPIC_API_KEY must be set for provider = \"anthropic\"")?;
        let model = model_for(config, self);
        let body = json!({
            "model": model,
            "max_tokens": 128,
            "temperature": 0.2,
            "system": config.prompt,
            "messages": [
                {
                    "role": "user",
                    "content": thread_content_for_request(thread_content)
                }
            ]
        })
        .to_string();

        let response = curl_post_json(
            ANTHROPIC_MESSAGES_URL,
            &[
                format!("x-api-key: {api_key}"),
                format!("anthropic-version: {ANTHROPIC_VERSION}"),
            ],
            body,
            self.name(),
        )?;
        let value = serde_json::from_str::<Value>(&response.body)?;
        if let Some(error) = api_error(&value, self.name()) {
            return Err(error.into());
        }
        let text = value
            .get("content")
            .and_then(|value| value.as_array())
            .and_then(|content| content.first())
            .and_then(|block| block.get("text"))
            .and_then(|text| text.as_str())
            .map(ToString::to_string)
            .ok_or("Anthropic response did not include content[0].text")?;
        Ok(TitleResponse {
            text,
            provider: self.id().to_string(),
            model,
            prompt: config.prompt.clone(),
            response_headers: response.headers,
            response_body: value,
        })
    }
}
