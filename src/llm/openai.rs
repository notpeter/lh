use serde_json::{Value, json};

use crate::common::LhResult;
use crate::config::LlmConfig;

use super::{LlmProvider, api_error, curl_post_json, model_for, thread_content_for_request};

const OPENAI_RESPONSES_URL: &str = "https://api.openai.com/v1/responses";

pub struct OpenAiProvider;

impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &'static str {
        "OpenAI"
    }

    fn default_model(&self) -> &'static str {
        "gpt-5.4-nano"
    }

    fn generate_title(&self, config: &LlmConfig, thread_content: &str) -> LhResult<String> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| "OPENAI_API_KEY must be set for provider = \"openai\"")?;
        let body = json!({
            "model": model_for(config, self),
            "instructions": config.prompt,
            "input": thread_content_for_request(thread_content),
            "max_output_tokens": 128,
            "store": false,
        })
        .to_string();

        let response = curl_post_json(
            OPENAI_RESPONSES_URL,
            &[format!("Authorization: Bearer {api_key}")],
            body,
            self.name(),
        )?;
        let value = serde_json::from_str::<Value>(&response)?;
        if let Some(error) = api_error(&value, self.name()) {
            return Err(error.into());
        }
        extract_output_text(&value)
    }
}

fn extract_output_text(value: &Value) -> LhResult<String> {
    if let Some(text) = value.get("output_text").and_then(|value| value.as_str()) {
        return Ok(text.to_string());
    }
    let output = value
        .get("output")
        .and_then(|value| value.as_array())
        .ok_or("OpenAI response did not include output[]")?;
    for item in output {
        let Some(content) = item.get("content").and_then(|value| value.as_array()) else {
            continue;
        };
        for block in content {
            if block.get("type").and_then(|value| value.as_str()) == Some("output_text")
                && let Some(text) = block.get("text").and_then(|value| value.as_str())
            {
                return Ok(text.to_string());
            }
        }
    }
    Err("OpenAI response did not include output text".into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn extracts_output_text() {
        let value = json!({
            "output": [
                {
                    "type": "message",
                    "content": [
                        {
                            "type": "output_text",
                            "text": "rename-thread-command"
                        }
                    ]
                }
            ]
        });
        assert_eq!(
            extract_output_text(&value).unwrap(),
            "rename-thread-command"
        );
    }
}
