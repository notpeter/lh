use std::io::Cursor;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, params};
use serde_json::Value;

use crate::common::{
    AgentKind, AgentProvider, LaunchCommand, LhResult, ThreadSummary, default_executable,
};
use crate::util::first_model_string_at_paths;
use crate::util::{canonicalize_existing, home_dir, parse_time, path_is_at_or_under};

pub struct ZedProvider {
    home: PathBuf,
}

impl ZedProvider {
    pub fn new() -> Self {
        Self { home: home_dir() }
    }

    #[cfg(test)]
    pub fn with_home(home: PathBuf) -> Self {
        Self { home }
    }

    fn data_dir(&self) -> PathBuf {
        #[cfg(target_os = "macos")]
        {
            self.home.join("Library/Application Support/Zed")
        }

        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        {
            std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| self.home.join(".local/share"))
                .join("zed")
        }

        #[cfg(target_os = "windows")]
        {
            std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| self.home.join("AppData/Local"))
                .join("Zed")
        }

        #[cfg(not(any(
            target_os = "macos",
            target_os = "linux",
            target_os = "freebsd",
            target_os = "windows"
        )))]
        {
            self.home.join(".config/zed")
        }
    }

    fn db_path(&self) -> PathBuf {
        self.data_dir().join("threads/threads.db")
    }
}

impl AgentProvider for ZedProvider {
    fn kind(&self) -> AgentKind {
        AgentKind::Zed
    }

    fn history_path(&self, _cwd: &Path) -> PathBuf {
        self.db_path()
    }

    fn executable(&self) -> Option<PathBuf> {
        crate::util::find_executable("zed")
    }

    fn list_threads(&self, cwd: &Path) -> LhResult<Vec<ThreadSummary>> {
        let canonical_cwd = canonicalize_existing(cwd);
        self.list_from_db(Some(&canonical_cwd))
    }

    fn list_threads_global(&self) -> LhResult<Vec<ThreadSummary>> {
        self.list_from_db(None)
    }

    fn new_command(&self, _name: Option<&str>, cwd: &Path) -> LhResult<LaunchCommand> {
        Ok(LaunchCommand::new(
            default_executable("zed"),
            [cwd.as_os_str().to_os_string()],
        ))
    }

    fn resume_command(&self, thread: Option<&ThreadSummary>) -> LhResult<LaunchCommand> {
        let args = thread
            .map(|thread| vec![thread.cwd.as_os_str().to_os_string()])
            .unwrap_or_default();
        Ok(LaunchCommand::new(default_executable("zed"), args))
    }

    fn thread_content(&self, thread: &ThreadSummary) -> LhResult<String> {
        let conn = Connection::open_with_flags(self.db_path(), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut stmt = conn.prepare("select data_type, data from threads where id = ?1 limit 1")?;
        let (data_type, data): (String, Vec<u8>) =
            stmt.query_row(params![thread.id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        zed_thread_content(&data_type, &data)
    }

    fn supports_move_thread(&self) -> bool {
        true
    }

    fn move_thread(&self, thread: &ThreadSummary, target_cwd: &Path) -> LhResult<()> {
        let conn = Connection::open(self.db_path())?;
        let changed = conn.execute(
            "update threads set folder_paths = ?1, folder_paths_order = '0' where id = ?2",
            params![target_cwd.display().to_string(), thread.id],
        )?;
        if changed == 0 {
            return Err(format!("Zed thread not found: {}", thread.id).into());
        }
        Ok(())
    }
}

impl ZedProvider {
    fn list_from_db(&self, cwd_filter: Option<&Path>) -> LhResult<Vec<ThreadSummary>> {
        let db_path = self.db_path();
        if !db_path.exists() {
            return Ok(Vec::new());
        }

        let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut stmt = conn.prepare(
            "select id, parent_id, folder_paths, folder_paths_order, summary, updated_at, created_at, data_type, data
             from threads
             order by updated_at desc, created_at desc",
        )?;
        let mut rows = stmt.query([])?;
        let mut threads = Vec::new();

        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let parent_id: Option<String> = row.get(1)?;
            if parent_id.is_some() {
                continue;
            }

            let folder_paths: Option<String> = row.get(2)?;
            let folder_paths_order: Option<String> = row.get(3)?;
            let paths = zed_folder_paths(folder_paths.as_deref(), folder_paths_order.as_deref());
            let canonical_paths = paths
                .iter()
                .map(|path| canonicalize_existing(path))
                .collect::<Vec<_>>();

            if let Some(cwd_filter) = cwd_filter
                && !canonical_paths
                    .iter()
                    .any(|path| path_is_at_or_under(path, cwd_filter))
            {
                continue;
            }

            let summary: String = row.get(4)?;
            let updated_at: String = row.get(5)?;
            let created_at: Option<String> = row.get(6)?;
            let data_type: String = row.get(7)?;
            let data: Vec<u8> = row.get(8)?;
            let preview = first_zed_text(&data_type, &data);
            let model = zed_model(&data_type, &data);
            let cwd = canonical_paths
                .first()
                .cloned()
                .unwrap_or_else(|| self.data_dir());

            threads.push(ThreadSummary {
                agent: AgentKind::Zed,
                id,
                name: (!summary.trim().is_empty()).then_some(summary),
                model,
                cwd,
                created_at: created_at.as_deref().and_then(parse_time),
                updated_at: parse_time(&updated_at),
                source_path: Some(db_path.clone()),
                preview,
                removable: None,
            });
        }

        threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_sort_key()));
        Ok(threads)
    }
}

fn zed_folder_paths(paths: Option<&str>, order: Option<&str>) -> Vec<PathBuf> {
    let Some(paths) = paths else {
        return Vec::new();
    };
    let mut paths = paths
        .split('\n')
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    let order = order
        .unwrap_or_default()
        .split(',')
        .filter_map(|index| index.parse::<usize>().ok())
        .collect::<Vec<_>>();

    if order.len() == paths.len() {
        let mut indexed = paths
            .into_iter()
            .zip(order)
            .map(|(path, index)| (index, path))
            .collect::<Vec<_>>();
        indexed.sort_by_key(|(index, _)| *index);
        paths = indexed.into_iter().map(|(_, path)| path).collect();
    } else {
        paths.sort();
    }

    paths
}

fn zed_thread_content(data_type: &str, data: &[u8]) -> LhResult<String> {
    let value = zed_thread_json(data_type, data)?;
    Ok(value
        .get("messages")
        .and_then(|messages| messages.as_array())
        .map(|messages| zed_messages_to_markdown(messages).trim().to_string())
        .filter(|content| !content.is_empty())
        .unwrap_or_else(|| serde_json::to_string_pretty(&value).unwrap_or_default()))
}

fn first_zed_text(data_type: &str, data: &[u8]) -> Option<String> {
    let value = zed_thread_json(data_type, data).ok()?;
    value
        .get("messages")?
        .as_array()?
        .iter()
        .find_map(first_text_in_message)
}

fn zed_model(data_type: &str, data: &[u8]) -> Option<String> {
    let value = zed_thread_json(data_type, data).ok()?;
    model_from_zed_value(&value).or_else(|| {
        value
            .get("messages")?
            .as_array()?
            .iter()
            .find_map(|message| message.get("Agent").and_then(model_from_zed_value))
    })
}

fn model_from_zed_value(value: &Value) -> Option<String> {
    first_model_string_at_paths(
        value,
        &[
            &["model"],
            &["model_id"],
            &["modelId"],
            &["modelID"],
            &["model", "id"],
            &["model", "name"],
            &["model", "model"],
            &["model", "model_id"],
            &["model", "modelId"],
            &["model", "modelID"],
        ],
    )
}

fn zed_thread_json(data_type: &str, data: &[u8]) -> LhResult<Value> {
    let json = match data_type {
        "zstd" => {
            let bytes = zstd::stream::decode_all(Cursor::new(data))?;
            String::from_utf8(bytes)?
        }
        "json" => String::from_utf8(data.to_vec())?,
        other => return Err(format!("unknown Zed thread data_type: {other}").into()),
    };
    Ok(serde_json::from_str(&json)?)
}

fn zed_messages_to_markdown(messages: &[Value]) -> String {
    let mut markdown = String::new();
    for (ix, message) in messages.iter().enumerate() {
        if ix > 0 {
            markdown.push('\n');
        }
        if message.get("User").is_some() {
            markdown.push_str("## User\n\n");
        } else if message.get("Agent").is_some() {
            markdown.push_str("## Assistant\n\n");
        }
        markdown.push_str(&zed_message_to_markdown(message));
    }
    markdown
}

fn zed_message_to_markdown(message: &Value) -> String {
    if let Some(user) = message.get("User") {
        return user
            .get("content")
            .and_then(|content| content.as_array())
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(user_content_to_markdown)
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
    }

    if let Some(agent) = message.get("Agent") {
        let mut markdown = agent
            .get("content")
            .and_then(|content| content.as_array())
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(agent_content_to_markdown)
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();

        if let Some(results) = agent
            .get("tool_results")
            .and_then(|results| results.as_object())
        {
            for result in results.values() {
                if let Some(text) = tool_result_to_markdown(result) {
                    markdown.push_str(&text);
                }
            }
        }

        return markdown;
    }

    if message.get("Resume").is_some() {
        return "[resume]\n".to_string();
    }
    if message.get("Compaction").is_some() {
        return "--- Context Compacted ---\n".to_string();
    }
    String::new()
}

fn first_text_in_message(message: &Value) -> Option<String> {
    let content = message
        .get("User")
        .or_else(|| message.get("Agent"))?
        .get("content")?
        .as_array()?;
    content.iter().find_map(first_text_in_content)
}

fn first_text_in_content(content: &Value) -> Option<String> {
    content
        .get("Text")
        .and_then(|text| text.as_str())
        .map(ToString::to_string)
        .or_else(|| {
            content
                .get("Mention")
                .and_then(|mention| mention.get("content"))
                .and_then(|text| text.as_str())
                .map(ToString::to_string)
        })
}

fn user_content_to_markdown(content: &Value) -> Option<String> {
    if let Some(text) = content.get("Text").and_then(|text| text.as_str()) {
        return Some(format!("{text}\n"));
    }
    if content.get("Image").is_some() {
        return Some("<image />\n".to_string());
    }
    if let Some(mention) = content.get("Mention") {
        let uri = mention
            .get("uri")
            .and_then(mention_uri_to_string)
            .unwrap_or_else(|| "<mention>".to_string());
        let body = mention.get("content").and_then(|text| text.as_str());
        return Some(match body {
            Some(body) if !body.is_empty() => format!("{uri}\n\n{body}\n"),
            _ => format!("{uri}\n"),
        });
    }
    None
}

fn agent_content_to_markdown(content: &Value) -> Option<String> {
    if let Some(text) = content.get("Text").and_then(|text| text.as_str()) {
        return Some(format!("{text}\n"));
    }
    if let Some(thinking) = content.get("Thinking") {
        let text = thinking.get("text").and_then(|text| text.as_str())?;
        return Some(format!("<think>{text}</think>\n"));
    }
    if content.get("RedactedThinking").is_some() {
        return Some("<redacted_thinking />\n".to_string());
    }
    if let Some(tool_use) = content.get("ToolUse") {
        let name = tool_use
            .get("name")
            .and_then(|name| name.as_str())
            .unwrap_or("<unknown>");
        let id = tool_use
            .get("id")
            .and_then(|id| id.as_str())
            .unwrap_or("<unknown>");
        let input = tool_use.get("input").unwrap_or(&Value::Null);
        let input = serde_json::to_string_pretty(input).unwrap_or_else(|_| "null".to_string());
        return Some(format!(
            "**Tool Use**: {name} (ID: {id})\n```json\n{input}\n```\n"
        ));
    }
    None
}

fn tool_result_to_markdown(result: &Value) -> Option<String> {
    let name = result
        .get("tool_name")
        .and_then(|name| name.as_str())
        .unwrap_or("<unknown>");
    let id = result
        .get("tool_use_id")
        .and_then(|id| id.as_str())
        .unwrap_or("<unknown>");
    let mut markdown = format!("**Tool Result**: {name} (ID: {id})\n\n");
    if result
        .get("is_error")
        .and_then(|is_error| is_error.as_bool())
        .unwrap_or(false)
    {
        markdown.push_str("**ERROR:**\n");
    }
    if let Some(parts) = result.get("content").and_then(|content| content.as_array()) {
        for part in parts {
            if let Some(text) = part
                .get("Text")
                .and_then(|text| text.as_str())
                .or_else(|| part.get("text").and_then(|text| text.as_str()))
            {
                markdown.push_str(text);
                markdown.push_str("\n\n");
            } else if part.get("Image").is_some() {
                markdown.push_str("<image />\n\n");
            }
        }
    }
    Some(markdown)
}

fn mention_uri_to_string(uri: &Value) -> Option<String> {
    uri.as_str()
        .map(ToString::to_string)
        .or_else(|| serde_json::to_string(uri).ok())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::util::temp_dir;

    #[test]
    fn lists_zed_threads_from_threads_db() {
        let root = temp_dir("zed-provider");
        let cwd = root.join("project");
        fs::create_dir_all(&cwd).unwrap();
        let provider = ZedProvider::with_home(root.clone());
        write_thread_db(
            &provider.db_path(),
            &[FixtureThread {
                id: "zed-1",
                summary: "Zed title",
                folder_paths: Some(cwd.to_string_lossy().as_ref()),
                folder_paths_order: Some("0"),
                created_at: Some("2026-05-01T00:00:00Z"),
                updated_at: "2026-05-01T00:05:00Z",
                parent_id: None,
                data_type: "zstd",
                data: zstd_thread_data("hello zed"),
            }],
        );

        let threads = provider.list_threads(&cwd).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].agent, AgentKind::Zed);
        assert_eq!(threads[0].id, "zed-1");
        assert_eq!(threads[0].name.as_deref(), Some("Zed title"));
        assert_eq!(threads[0].model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(threads[0].preview.as_deref(), Some("hello zed"));
        assert_eq!(threads[0].cwd, canonicalize_existing(&cwd));
    }

    #[test]
    fn skips_zed_subagent_threads() {
        let root = temp_dir("zed-subagent");
        let cwd = root.join("project");
        fs::create_dir_all(&cwd).unwrap();
        let provider = ZedProvider::with_home(root);
        write_thread_db(
            &provider.db_path(),
            &[FixtureThread {
                id: "child",
                summary: "Child",
                folder_paths: Some(cwd.to_string_lossy().as_ref()),
                folder_paths_order: Some("0"),
                created_at: Some("2026-05-01T00:00:00Z"),
                updated_at: "2026-05-01T00:05:00Z",
                parent_id: Some("parent"),
                data_type: "zstd",
                data: zstd_thread_data("hidden"),
            }],
        );

        assert!(provider.list_threads_global().unwrap().is_empty());
    }

    #[test]
    fn renders_zed_thread_content() {
        let data = zstd_thread_data("hello zed");
        let markdown = zed_thread_content("zstd", &data).unwrap();
        assert!(markdown.contains("## User\n\nhello zed"));
        assert!(markdown.contains("## Assistant\n\nhi back"));
    }

    #[test]
    fn moves_zed_thread_to_target_cwd() {
        let root = temp_dir("zed-move");
        let cwd = root.join("project");
        let target = root.join("target");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&target).unwrap();
        let provider = ZedProvider::with_home(root);
        write_thread_db(
            &provider.db_path(),
            &[FixtureThread {
                id: "zed-1",
                summary: "Zed title",
                folder_paths: Some(cwd.to_string_lossy().as_ref()),
                folder_paths_order: Some("0"),
                created_at: Some("2026-05-01T00:00:00Z"),
                updated_at: "2026-05-01T00:05:00Z",
                parent_id: None,
                data_type: "zstd",
                data: zstd_thread_data("hello zed"),
            }],
        );
        let thread = provider.list_threads(&cwd).unwrap().remove(0);

        provider.move_thread(&thread, &target).unwrap();

        assert!(provider.list_threads(&cwd).unwrap().is_empty());
        let moved = provider.list_threads(&target).unwrap().remove(0);
        assert_eq!(moved.id, "zed-1");
        assert_eq!(moved.cwd, canonicalize_existing(&target));
    }

    struct FixtureThread<'a> {
        id: &'a str,
        summary: &'a str,
        folder_paths: Option<&'a str>,
        folder_paths_order: Option<&'a str>,
        created_at: Option<&'a str>,
        updated_at: &'a str,
        parent_id: Option<&'a str>,
        data_type: &'a str,
        data: Vec<u8>,
    }

    fn write_thread_db(path: &Path, threads: &[FixtureThread<'_>]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "create table threads (
                id text primary key,
                summary text not null,
                updated_at text not null,
                data_type text not null,
                data blob not null,
                parent_id text,
                folder_paths text,
                folder_paths_order text,
                created_at text
            )",
        )
        .unwrap();
        for thread in threads {
            conn.execute(
                "insert into threads (id, summary, updated_at, data_type, data, parent_id, folder_paths, folder_paths_order, created_at)
                 values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    thread.id,
                    thread.summary,
                    thread.updated_at,
                    thread.data_type,
                    thread.data,
                    thread.parent_id,
                    thread.folder_paths,
                    thread.folder_paths_order,
                    thread.created_at,
                ],
            )
            .unwrap();
        }
    }

    fn zstd_thread_data(first_user_text: &str) -> Vec<u8> {
        let value = serde_json::json!({
            "version": "0.3.0",
            "title": "Zed title",
            "model": { "id": "claude-sonnet-4-5" },
            "updated_at": "2026-05-01T00:05:00Z",
            "messages": [
                {
                    "User": {
                        "id": "user-1",
                        "content": [
                            { "Text": first_user_text }
                        ]
                    }
                },
                {
                    "Agent": {
                        "content": [
                            { "Text": "hi back" }
                        ],
                        "tool_results": {},
                        "reasoning_details": null
                    }
                }
            ]
        });
        zstd::stream::encode_all(Cursor::new(value.to_string().into_bytes()), 3).unwrap()
    }
}
