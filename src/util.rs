use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

pub fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn canonicalize_existing(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub fn find_executable(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(name);
        candidate.is_file().then_some(candidate)
    })
}

pub fn collect_files_with_name_prefix(root: &Path, prefix: &str, suffix: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_files(root, &mut |path| {
        if let Some(name) = path.file_name().and_then(|name| name.to_str())
            && name.starts_with(prefix)
            && name.ends_with(suffix)
        {
            out.push(path.to_path_buf());
        }
    });
    out
}

pub fn collect_files(root: &Path, visitor: &mut impl FnMut(&Path)) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, visitor);
        } else if path.is_file() {
            visitor(&path);
        }
    }
}

pub fn parse_time(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

pub fn format_time(value: OffsetDateTime) -> String {
    value
        .to_offset(UtcOffset::UTC)
        .format(&Rfc3339)
        .unwrap_or_else(|_| value.unix_timestamp().to_string())
}

pub fn format_display_time(value: OffsetDateTime) -> String {
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    let format = time::format_description::parse("[year]-[month]-[day] [hour]:[minute]:[second]");
    match format {
        Ok(format) => value
            .to_offset(offset)
            .format(&format)
            .unwrap_or_else(|_| value.unix_timestamp().to_string()),
        Err(_) => value.unix_timestamp().to_string(),
    }
}

pub fn millis_to_time(value: i64) -> Option<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp(value / 1000).ok()
}

pub fn read_to_string(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

pub fn shorten_path(path: &Path) -> String {
    let home = home_dir();
    if let Ok(stripped) = path.strip_prefix(&home) {
        format!("~/{}", stripped.display())
    } else {
        path.display().to_string()
    }
}

pub fn first_json_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(items) => items.iter().find_map(|item| {
            item.get("text")
                .and_then(|text| text.as_str())
                .or_else(|| item.get("content").and_then(|text| text.as_str()))
                .map(ToString::to_string)
        }),
        serde_json::Value::Object(map) => map
            .get("text")
            .and_then(|text| text.as_str())
            .or_else(|| map.get("content").and_then(|text| text.as_str()))
            .map(ToString::to_string),
        _ => None,
    }
}

#[cfg(test)]
pub fn temp_dir(name: &str) -> PathBuf {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = env::temp_dir().join(format!("lh-test-{name}-{}-{now}", std::process::id()));
    fs::create_dir_all(&path).unwrap();
    path
}
