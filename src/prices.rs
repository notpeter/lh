use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::common::LhResult;
use crate::util::{APP_DIR_NAME, format_time, home_dir};

const MODEL_PRICES_URL: &str = "https://raw.githubusercontent.com/BerriAI/litellm/refs/heads/main/model_prices_and_context_window.json";
const MODEL_PRICES_FILE: &str = "model_prices_and_context_window.json";
const PRICE_CACHE_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, PartialEq)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: Option<u64>,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RequestCost {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: Option<u64>,
    pub input_cost_usd: f64,
    pub output_cost_usd: f64,
    pub total_cost_usd: f64,
}

pub struct RenameRequestRecord<'a> {
    pub date: OffsetDateTime,
    pub provider: &'a str,
    pub model: &'a str,
    pub prompt: &'a str,
    pub path: &'a Path,
    pub agent: &'a str,
    pub id: &'a str,
    pub bytes: usize,
    pub response_headers: &'a BTreeMap<String, Vec<String>>,
    pub response_body: &'a Value,
}

pub fn cache_dir() -> PathBuf {
    cache_dir_for(
        &home_dir(),
        std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from),
        std::env::consts::OS,
    )
}

pub fn cache_dir_for(home: &Path, xdg_cache_home: Option<PathBuf>, os: &str) -> PathBuf {
    match os {
        "macos" => home.join("Library/Caches").join(APP_DIR_NAME),
        _ => xdg_cache_home
            .map(|path| path.join(APP_DIR_NAME))
            .unwrap_or_else(|| home.join(".cache").join(APP_DIR_NAME)),
    }
}

pub fn ensure_model_prices_cache() -> LhResult<PathBuf> {
    let path = cache_dir().join(MODEL_PRICES_FILE);
    if is_fresh(&path)? {
        return Ok(path);
    }

    match fetch_model_prices() {
        Ok(text) => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, text)?;
            Ok(path)
        }
        Err(error) if path.exists() => {
            eprintln!(
                "warning: failed to refresh model prices, using cached copy: {}",
                error
            );
            Ok(path)
        }
        Err(error) => Err(error),
    }
}

pub fn record_rename_request(record: &RenameRequestRecord<'_>) -> LhResult<PathBuf> {
    let dir = cache_dir().join("renames");
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!(
        "{}_{}.jsonl",
        request_file_timestamp(record.date),
        request_file_component(record.provider)
    ));
    let value = json!({
        "request": {
            "date": format_time(record.date),
            "provider": record.provider,
            "model": record.model,
            "prompt": record.prompt,
            "path": record.path.to_string_lossy(),
            "agent": record.agent,
            "id": record.id,
            "bytes": record.bytes,
        },
        "response": {
            "headers": record.response_headers,
            "body": record.response_body,
        },
    });
    fs::write(&path, format!("{}\n", serde_json::to_string(&value)?))?;
    Ok(path)
}

pub fn estimate_request_cost(
    provider: &str,
    model: &str,
    headers: &BTreeMap<String, Vec<String>>,
    body: &Value,
) -> LhResult<Option<RequestCost>> {
    let Some(usage) = extract_usage(headers, body) else {
        return Ok(None);
    };
    let prices_path = ensure_model_prices_cache()?;
    let prices = serde_json::from_str::<Value>(&fs::read_to_string(prices_path)?)?;
    let Some((priced_model, price)) = price_for_request(&prices, provider, model, body) else {
        return Ok(None);
    };

    let input_cost_per_token = number_field(price, "input_cost_per_token").unwrap_or_default();
    let output_cost_per_token = number_field(price, "output_cost_per_token").unwrap_or_default();
    let cache_read_cost_per_token = number_field(price, "cache_read_input_token_cost")
        .or_else(|| number_field(price, "cache_read_input_cost_per_token"))
        .unwrap_or(input_cost_per_token);
    let cache_creation_cost_per_token = number_field(price, "cache_creation_input_token_cost")
        .or_else(|| number_field(price, "cache_creation_input_cost_per_token"))
        .unwrap_or(input_cost_per_token);

    let cache_tokens = usage
        .cache_read_input_tokens
        .saturating_add(usage.cache_creation_input_tokens);
    let regular_input_tokens = usage.input_tokens.saturating_sub(cache_tokens);
    let input_cost_usd = regular_input_tokens as f64 * input_cost_per_token
        + usage.cache_read_input_tokens as f64 * cache_read_cost_per_token
        + usage.cache_creation_input_tokens as f64 * cache_creation_cost_per_token;
    let output_cost_usd = usage.output_tokens as f64 * output_cost_per_token;
    let total_cost_usd = input_cost_usd + output_cost_usd;

    Ok(Some(RequestCost {
        model: priced_model,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        total_tokens: usage.total_tokens,
        input_cost_usd,
        output_cost_usd,
        total_cost_usd,
    }))
}

pub fn extract_usage(headers: &BTreeMap<String, Vec<String>>, body: &Value) -> Option<TokenUsage> {
    usage_from_headers(headers).or_else(|| usage_from_body(body))
}

fn is_fresh(path: &Path) -> LhResult<bool> {
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(false);
    };
    let modified = metadata.modified()?;
    let age = SystemTime::now()
        .duration_since(modified)
        .unwrap_or(Duration::ZERO);
    Ok(age <= PRICE_CACHE_MAX_AGE)
}

fn fetch_model_prices() -> LhResult<String> {
    let output = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--max-time",
            "120",
            MODEL_PRICES_URL,
        ])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("failed to fetch model prices: {stderr}").into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn price_for_model<'a>(
    prices: &'a Value,
    provider: &str,
    model: &str,
) -> Option<(String, &'a Value)> {
    let object = prices.as_object()?;
    for candidate in model_candidates(provider, model) {
        if let Some(value) = object.get(&candidate) {
            return Some((candidate, value));
        }
    }

    let normalized_provider = normalize_provider(provider);
    let normalized_model = normalize_model(model);
    object.iter().find_map(|(name, value)| {
        let entry_provider = value
            .get("litellm_provider")
            .and_then(|value| value.as_str())
            .map(normalize_provider)?;
        if entry_provider != normalized_provider {
            return None;
        }
        let normalized_name = normalize_model(name);
        (normalized_name == normalized_model
            || normalized_name.ends_with(&format!("/{normalized_model}")))
        .then(|| (name.clone(), value))
    })
}

fn price_for_request<'a>(
    prices: &'a Value,
    provider: &str,
    requested_model: &str,
    body: &Value,
) -> Option<(String, &'a Value)> {
    std::iter::once(requested_model)
        .chain(body.get("model").and_then(|value| value.as_str()))
        .find_map(|model| price_for_model(prices, provider, model))
}

fn model_candidates(provider: &str, model: &str) -> Vec<String> {
    let normalized_provider = normalize_provider(provider);
    let normalized_model = normalize_model(model);
    let mut candidates = vec![model.to_string(), normalized_model.clone()];
    candidates.push(format!("{normalized_provider}/{normalized_model}"));
    if normalized_provider == "gemini" {
        candidates.push(format!("gemini/{normalized_model}"));
        candidates.push(format!("google/{normalized_model}"));
    }
    candidates.sort();
    candidates.dedup();
    candidates
}

fn normalize_provider(provider: &str) -> String {
    match provider.to_ascii_lowercase().as_str() {
        "openai" => "openai".to_string(),
        "anthropic" => "anthropic".to_string(),
        "gemini" | "google" | "google_ai_studio" | "google-ai-studio" => "gemini".to_string(),
        other => other.to_string(),
    }
}

fn normalize_model(model: &str) -> String {
    model
        .strip_prefix("models/")
        .unwrap_or(model)
        .to_ascii_lowercase()
}

fn usage_from_headers(headers: &BTreeMap<String, Vec<String>>) -> Option<TokenUsage> {
    if let Some(value) = header_string(headers, "x-litellm-usage")
        && let Ok(value) = serde_json::from_str::<Value>(&value)
        && let Some(usage) = usage_from_body(&json!({ "usage": value }))
    {
        return Some(usage);
    }

    let input_tokens = header_u64(headers, &["x-litellm-prompt-tokens", "x-prompt-tokens"])?;
    let output_tokens = header_u64(
        headers,
        &["x-litellm-completion-tokens", "x-completion-tokens"],
    )
    .unwrap_or_default();
    Some(TokenUsage {
        input_tokens,
        output_tokens,
        total_tokens: header_u64(headers, &["x-litellm-total-tokens", "x-total-tokens"]),
        cache_read_input_tokens: header_u64(
            headers,
            &[
                "x-litellm-cache-read-input-tokens",
                "x-cache-read-input-tokens",
            ],
        )
        .unwrap_or_default(),
        cache_creation_input_tokens: header_u64(
            headers,
            &[
                "x-litellm-cache-creation-input-tokens",
                "x-cache-creation-input-tokens",
            ],
        )
        .unwrap_or_default(),
    })
}

fn usage_from_body(body: &Value) -> Option<TokenUsage> {
    let usage = body.get("usage").or_else(|| body.get("usageMetadata"))?;
    let input_tokens = first_u64(
        usage,
        &[
            "input_tokens",
            "prompt_tokens",
            "promptTokenCount",
            "inputTokens",
        ],
    )?;
    let output_tokens = first_u64(
        usage,
        &[
            "output_tokens",
            "completion_tokens",
            "candidatesTokenCount",
            "outputTokens",
        ],
    )
    .unwrap_or_default()
        + reasoning_output_tokens(usage);
    let total_tokens = first_u64(usage, &["total_tokens", "totalTokenCount", "totalTokens"]);
    let cache_read_input_tokens = first_u64(
        usage,
        &[
            "cache_read_input_tokens",
            "cached_tokens",
            "cachedContentTokenCount",
        ],
    )
    .or_else(|| {
        usage
            .get("prompt_tokens_details")
            .and_then(|details| first_u64(details, &["cached_tokens"]))
    })
    .or_else(|| {
        usage
            .get("input_tokens_details")
            .and_then(|details| first_u64(details, &["cached_tokens"]))
    })
    .unwrap_or_default();
    let cache_creation_input_tokens =
        first_u64(usage, &["cache_creation_input_tokens"]).unwrap_or_default();

    Some(TokenUsage {
        input_tokens,
        output_tokens,
        total_tokens,
        cache_read_input_tokens,
        cache_creation_input_tokens,
    })
}

fn first_u64(value: &Value, names: &[&str]) -> Option<u64> {
    names.iter().find_map(|name| {
        value
            .get(*name)
            .and_then(|value| value.as_u64())
            .or_else(|| {
                value
                    .get(*name)
                    .and_then(|value| value.as_str())?
                    .parse()
                    .ok()
            })
    })
}

fn reasoning_output_tokens(usage: &Value) -> u64 {
    usage
        .get("completion_tokens_details")
        .or_else(|| usage.get("output_tokens_details"))
        .and_then(|details| first_u64(details, &["reasoning_tokens"]))
        .unwrap_or_default()
}

fn header_u64(headers: &BTreeMap<String, Vec<String>>, names: &[&str]) -> Option<u64> {
    names
        .iter()
        .find_map(|name| header_string(headers, name)?.parse().ok())
}

fn header_string(headers: &BTreeMap<String, Vec<String>>, name: &str) -> Option<String> {
    headers
        .get(&name.to_ascii_lowercase())
        .and_then(|values| values.last())
        .map(|value| value.trim().to_string())
}

fn number_field(value: &Value, name: &str) -> Option<f64> {
    value.get(name).and_then(|value| value.as_f64())
}

fn request_file_timestamp(date: OffsetDateTime) -> String {
    let format = time::format_description::parse(
        "[year]-[month]-[day]T[hour][minute][second].[subsecond digits:3]Z",
    );
    match format {
        Ok(format) => date
            .to_offset(time::UtcOffset::UTC)
            .format(&format)
            .unwrap_or_else(|_| date.unix_timestamp().to_string()),
        Err(_) => date.unix_timestamp().to_string(),
    }
}

fn request_file_component(value: &str) -> String {
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
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "llm".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::*;

    #[test]
    fn chooses_macos_cache_dir() {
        assert_eq!(
            cache_dir_for(Path::new("/home/me"), None, "macos"),
            PathBuf::from("/home/me/Library/Caches/llm-history")
        );
    }

    #[test]
    fn chooses_xdg_cache_dir() {
        assert_eq!(
            cache_dir_for(
                Path::new("/home/me"),
                Some(PathBuf::from("/tmp/cache")),
                "linux"
            ),
            PathBuf::from("/tmp/cache/llm-history")
        );
    }

    #[test]
    fn extracts_usage_from_openai_body() {
        let usage = extract_usage(
            &BTreeMap::new(),
            &json!({
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 20,
                    "total_tokens": 120
                }
            }),
        )
        .unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 20);
        assert_eq!(usage.total_tokens, Some(120));
    }

    #[test]
    fn extracts_usage_from_gemini_body() {
        let usage = extract_usage(
            &BTreeMap::new(),
            &json!({
                "usageMetadata": {
                    "promptTokenCount": 10,
                    "candidatesTokenCount": 5,
                    "totalTokenCount": 15
                }
            }),
        )
        .unwrap();
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.total_tokens, Some(15));
    }

    #[test]
    fn extracts_usage_from_litellm_headers() {
        let mut headers = BTreeMap::new();
        headers.insert(
            "x-litellm-prompt-tokens".to_string(),
            vec!["12".to_string()],
        );
        headers.insert(
            "x-litellm-completion-tokens".to_string(),
            vec!["3".to_string()],
        );
        let usage = extract_usage(&headers, &json!({})).unwrap();
        assert_eq!(usage.input_tokens, 12);
        assert_eq!(usage.output_tokens, 3);
    }

    #[test]
    fn prices_resolved_response_model_when_request_used_alias() {
        let prices = json!({
            "openai/gpt-5.4-nano": {
                "litellm_provider": "openai",
                "input_cost_per_token": 0.00000125,
                "output_cost_per_token": 0.0000025
            }
        });
        let (model, _) = price_for_request(
            &prices,
            "openai",
            "latest",
            &json!({
                "model": "gpt-5.4-nano"
            }),
        )
        .unwrap();
        assert_eq!(model, "openai/gpt-5.4-nano");
    }
}
