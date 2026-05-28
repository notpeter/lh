use serde_json::{Value, json};

use crate::common::LhResult;
use crate::config::LlmConfig;

use super::{LlmProvider, api_error, curl_post_json, model_for, thread_content_for_request};

const GEMINI_GENERATE_CONTENT_BASE_URL: &str =
    "https://generativelanguage.googleapis.com/v1beta/models";

pub struct GeminiProvider;

impl LlmProvider for GeminiProvider {
    fn name(&self) -> &'static str {
        "Gemini"
    }

    fn default_model(&self) -> &'static str {
        "gemini-3.1-flash-lite"
    }

    fn generate_title(&self, config: &LlmConfig, thread_content: &str) -> LhResult<String> {
        let api_key = std::env::var("GEMINI_API_KEY")
            .map_err(|_| "GEMINI_API_KEY must be set for provider = \"gemini\"")?;
        let body = json!({
            "system_instruction": {
                "parts": [
                    {
                        "text": config.prompt
                    }
                ]
            },
            "contents": [
                {
                    "parts": [
                        {
                            "text": thread_content_for_request(thread_content)
                        }
                    ]
                }
            ],
            "generationConfig": {
                "temperature": 0.2,
                "maxOutputTokens": 128
            }
        })
        .to_string();
        let url = format!(
            "{}/{}:generateContent",
            GEMINI_GENERATE_CONTENT_BASE_URL,
            model_for(config, self)
        );

        let response = curl_post_json(
            &url,
            &[format!("x-goog-api-key: {api_key}")],
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
    let candidates = value
        .get("candidates")
        .and_then(|value| value.as_array())
        .ok_or("Gemini response did not include candidates[]")?;
    for candidate in candidates {
        let Some(parts) = candidate
            .get("content")
            .and_then(|content| content.get("parts"))
            .and_then(|parts| parts.as_array())
        else {
            continue;
        };
        for part in parts {
            if let Some(text) = part.get("text").and_then(|text| text.as_str()) {
                return Ok(text.to_string());
            }
        }
    }
    Err("Gemini response did not include candidate text".into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn extracts_output_text() {
        let value = json!({
            "candidates": [
                {
                    "content": {
                        "parts": [
                            {
                                "text": "gemini-generated-title"
                            }
                        ]
                    }
                }
            ]
        });
        assert_eq!(
            extract_output_text(&value).unwrap(),
            "gemini-generated-title"
        );
    }
}
