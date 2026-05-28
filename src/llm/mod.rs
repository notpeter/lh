use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::common::LhResult;
use crate::config::{Config, LlmConfig};

mod anthropic;
mod gemini;
mod openai;
mod xai;

const MAX_THREAD_CHARS: usize = 40_000;

pub trait LlmProvider {
    fn name(&self) -> &'static str;
    fn default_model(&self) -> &'static str;
    fn generate_title(&self, config: &LlmConfig, thread_content: &str) -> LhResult<String>;
}

pub fn generate_thread_name(config: &Config, thread_content: &str) -> LhResult<String> {
    let llm = config
        .llm
        .as_ref()
        .ok_or("no [llm] config found for --auto rename")?;
    let provider = provider_for(&llm.provider)?;
    let text = provider.generate_title(llm, thread_content)?;
    clean_generated_name(&text).ok_or_else(|| "llm returned an empty title".into())
}

fn provider_for(name: &str) -> LhResult<Box<dyn LlmProvider>> {
    match name {
        "anthropic" => Ok(Box::new(anthropic::AnthropicProvider)),
        "openai" => Ok(Box::new(openai::OpenAiProvider)),
        "xai" | "x_api" => Ok(Box::new(xai::XaiProvider)),
        "gemini" => Ok(Box::new(gemini::GeminiProvider)),
        _ => Err(format!("unsupported llm provider '{name}'").into()),
    }
}

fn model_for(config: &LlmConfig, provider: &dyn LlmProvider) -> String {
    config
        .model
        .clone()
        .unwrap_or_else(|| provider.default_model().to_string())
}

fn thread_content_for_request(thread_content: &str) -> String {
    truncate_chars(thread_content, MAX_THREAD_CHARS)
}

fn curl_post_json(
    url: &str,
    headers: &[String],
    body: String,
    provider_name: &str,
) -> LhResult<String> {
    let body_path = temp_body_path();
    fs::write(&body_path, body)?;
    let headers = headers
        .iter()
        .map(|header| format!("header = {}\n", curl_quote(header)))
        .collect::<String>();
    let curl_config = format!(
        "url = {}\nrequest = \"POST\"\nsilent\nshow-error\nfail-with-body\nmax-time = 120\n{}header = \"content-type: application/json\"\ndata = {}\n",
        curl_quote(url),
        headers,
        curl_quote(&format!("@{}", body_path.display())),
    );

    let mut child = Command::new("curl")
        .arg("--config")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    {
        let stdin = child.stdin.as_mut().ok_or("failed to open curl stdin")?;
        stdin.write_all(curl_config.as_bytes())?;
    }
    drop(child.stdin.take());

    let output = child.wait_with_output()?;
    let _ = fs::remove_file(&body_path);
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{provider_name} request failed: {stderr}{stdout}").into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn temp_body_path() -> PathBuf {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("lh-llm-body-{}-{now}.json", std::process::id()))
}

fn curl_quote(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect()
}

fn clean_generated_name(value: &str) -> Option<String> {
    let value = value
        .trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .lines()
        .next()
        .unwrap_or_default();
    let mut out = String::new();
    let mut last_was_dash = false;

    for ch in value.chars().flat_map(|ch| ch.to_lowercase()) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }

    let mut out = out.trim_matches('-').to_string();
    if out.chars().count() > 60 {
        out = out.chars().take(60).collect::<String>();
        out = out.trim_matches('-').to_string();
    }
    (!out.is_empty()).then_some(out)
}

fn api_error(value: &Value, provider_name: &str) -> Option<String> {
    value
        .get("error")
        .and_then(|error| error.get("message"))
        .map(|message| format!("{provider_name} API error: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LlmConfig;

    #[test]
    fn cleans_generated_name() {
        assert_eq!(
            clean_generated_name("\"Fix parser title handling\""),
            Some("fix-parser-title-handling".to_string())
        );
    }

    #[test]
    fn truncates_thread_content() {
        assert_eq!(truncate_chars("abcdef", 3), "abc");
    }

    #[test]
    fn quotes_curl_config_values() {
        assert_eq!(curl_quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }

    #[test]
    fn model_defaults_by_provider() {
        for (provider, expected) in [
            ("anthropic", "claude-haiku-4-5"),
            ("openai", "gpt-5.4-nano"),
            ("xai", "grok-latest"),
            ("gemini", "gemini-3.1-flash-lite"),
        ] {
            let parsed = toml::from_str::<LlmConfig>(&format!(
                "provider = \"{provider}\"\nprompt = \"name it\"\n"
            ))
            .unwrap();
            let provider = provider_for(provider).unwrap();
            assert_eq!(model_for(&parsed, &*provider), expected);
        }
    }
}
