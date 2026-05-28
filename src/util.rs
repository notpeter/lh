use std::env;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

pub const APP_DIR_NAME: &str = "llm-history";

pub fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn canonicalize_existing(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub fn path_is_at_or_under(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
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

pub fn terminal_width() -> usize {
    if let Some(columns) = terminal_width_from_ioctl() {
        return columns;
    }

    if let Ok(columns) = env::var("COLUMNS")
        && let Ok(columns) = columns.parse::<usize>()
        && columns > 0
    {
        return columns;
    }

    120
}

#[cfg(unix)]
fn terminal_width_from_ioctl() -> Option<usize> {
    #[repr(C)]
    struct Winsize {
        ws_row: u16,
        ws_col: u16,
        ws_xpixel: u16,
        ws_ypixel: u16,
    }

    unsafe extern "C" {
        fn ioctl(fd: i32, request: usize, ...) -> i32;
    }

    #[cfg(target_os = "linux")]
    const TIOCGWINSZ: usize = 0x5413;
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    const TIOCGWINSZ: usize = 0x40087468;

    fn width_for_fd(fd: i32) -> Option<usize> {
        let mut size = Winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let result = unsafe { ioctl(fd, TIOCGWINSZ, &mut size) };
        (result == 0 && size.ws_col > 0).then_some(size.ws_col as usize)
    }

    let fds = [
        (std::io::stdout().is_terminal(), 1),
        (std::io::stderr().is_terminal(), 2),
        (std::io::stdin().is_terminal(), 0),
    ];
    fds.into_iter()
        .filter_map(|(is_terminal, fd)| is_terminal.then_some(fd))
        .find_map(width_for_fd)
}

#[cfg(not(unix))]
fn terminal_width_from_ioctl() -> Option<usize> {
    None
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
