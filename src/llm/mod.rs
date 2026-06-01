use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;
use time::OffsetDateTime;

use crate::common::{LhResult, ThreadSummary};
use crate::config::{Config, LlmConfig};
use crate::prices::{self, RenameRequestRecord, RequestCost};

mod anthropic;
mod gemini;
mod openai;

const MAX_THREAD_CHARS: usize = 40_000;

pub trait LlmProvider {
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn default_model(&self) -> &'static str;
    fn generate_title(&self, config: &LlmConfig, thread_content: &str) -> LhResult<TitleResponse>;
}

pub struct GeneratedThreadName {
    pub name: String,
    pub pricing: Option<RequestCost>,
}

pub struct TitleResponse {
    pub text: String,
    pub provider: String,
    pub model: String,
    pub prompt: String,
    pub response_headers: BTreeMap<String, Vec<String>>,
    pub response_body: Value,
}

pub struct CurlResponse {
    pub headers: BTreeMap<String, Vec<String>>,
    pub body: String,
}

pub fn generate_thread_name_for_rename(
    config: &Config,
    thread: &ThreadSummary,
    thread_content: &str,
) -> LhResult<GeneratedThreadName> {
    let llm = config
        .llm
        .as_ref()
        .ok_or("no [llm] config found for --auto rename")?;
    let provider = provider_for(&llm.provider)?;
    let date = OffsetDateTime::now_utc();
    let response = provider.generate_title(llm, thread_content)?;
    let name = clean_generated_name(&response.text).ok_or("llm returned an empty title")?;

    let record = RenameRequestRecord {
        date,
        provider: &response.provider,
        model: &response.model,
        prompt: &response.prompt,
        path: &thread.cwd,
        agent: thread.agent.as_str(),
        id: &thread.id,
        bytes: thread_content.len(),
        response_headers: &response.response_headers,
        response_body: &response.response_body,
    };
    if let Err(error) = prices::record_rename_request(&record) {
        eprintln!("warning: failed to record rename llm request: {error}");
    }

    let pricing = match prices::estimate_request_cost(
        &response.provider,
        &response.model,
        &response.response_headers,
        &response.response_body,
    ) {
        Ok(pricing) => pricing,
        Err(error) => {
            eprintln!("warning: failed to estimate rename llm cost: {error}");
            None
        }
    };

    Ok(GeneratedThreadName { name, pricing })
}

fn provider_for(name: &str) -> LhResult<Box<dyn LlmProvider>> {
    match name {
        "anthropic" => Ok(Box::new(anthropic::AnthropicProvider)),
        "openai" => Ok(Box::new(openai::OpenAiProvider)),
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
) -> LhResult<CurlResponse> {
    let body_path = temp_body_path();
    let header_path = temp_header_path();
    fs::write(&body_path, body)?;
    let headers = headers
        .iter()
        .map(|header| format!("header = {}\n", curl_quote(header)))
        .collect::<String>();
    let curl_config = format!(
        "url = {}\nrequest = \"POST\"\nsilent\nshow-error\nfail-with-body\nmax-time = 120\ndump-header = {}\n{}header = \"content-type: application/json\"\ndata = {}\n",
        curl_quote(url),
        curl_quote(&header_path.display().to_string()),
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
    let header_text = fs::read_to_string(&header_path).unwrap_or_default();
    let _ = fs::remove_file(&header_path);
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{provider_name} request failed: {stderr}{stdout}").into());
    }
    Ok(CurlResponse {
        headers: parse_header_dump(&header_text),
        body: String::from_utf8(output.stdout)?,
    })
}

fn temp_body_path() -> PathBuf {
    temp_path("body", "json")
}

fn temp_header_path() -> PathBuf {
    temp_path("headers", "txt")
}

fn temp_path(kind: &str, extension: &str) -> PathBuf {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!(
        "lh-llm-{kind}-{}-{now}.{extension}",
        std::process::id()
    ))
}

fn parse_header_dump(value: &str) -> BTreeMap<String, Vec<String>> {
    let mut headers = BTreeMap::<String, Vec<String>>::new();
    for line in value.lines() {
        if line.starts_with("HTTP/") {
            headers.clear();
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        headers
            .entry(name.trim().to_ascii_lowercase())
            .or_default()
            .push(value.trim().to_string());
    }
    headers
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
    fn parses_final_header_dump() {
        let headers = parse_header_dump(
            "HTTP/1.1 100 Continue\r\nx-old: nope\r\n\r\nHTTP/2 200\r\nX-Request-Id: abc\r\nx-request-id: def\r\n\r\n",
        );
        assert_eq!(
            headers.get("x-request-id").unwrap(),
            &vec!["abc".to_string(), "def".to_string()]
        );
        assert!(!headers.contains_key("x-old"));
    }

    #[test]
    fn model_defaults_by_provider() {
        for (provider, expected) in [
            ("anthropic", "claude-haiku-4-5"),
            ("openai", "gpt-5.4-nano"),
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
