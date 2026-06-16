use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use chrono::Utc;

use crate::models::*;
use crate::parsers::{
    folder_name_from_path, title_from_user_text, AgentParser, ParseError,
};

pub struct QoderCliParser {
    base_dir: PathBuf,
}

impl Default for QoderCliParser {
    fn default() -> Self {
        Self::new()
    }
}

impl QoderCliParser {
    pub fn new() -> Self {
        Self {
            base_dir: resolve_qodercli_base_dir(),
        }
    }

    fn projects_dir(&self) -> PathBuf {
        self.base_dir.join("projects")
    }

    fn list_jsonl_files(&self) -> Vec<PathBuf> {
        let projects = self.projects_dir();
        if !projects.exists() {
            return Vec::new();
        }
        let mut files = Vec::new();
        if let Ok(entries) = fs::read_dir(&projects) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Ok(inner) = fs::read_dir(&path) {
                        for f in inner.flatten() {
                            let fp = f.path();
                            if fp.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                                files.push(fp);
                            }
                        }
                    }
                }
            }
        }
        files
    }

    fn parse_jsonl_file(
        &self,
        path: &std::path::Path,
    ) -> Option<(ConversationSummary, Vec<MessageTurn>)> {
        let file = fs::File::open(path).ok()?;
        let reader = BufReader::new(file);

        let mut session_id: Option<String> = None;
        let mut model: Option<String> = None;
        let mut turns: Vec<MessageTurn> = Vec::new();
        let mut current_user_content: Vec<ContentBlock> = Vec::new();
        let mut cwd: Option<String> = None;
        let mut git_branch: Option<String> = None;
        let mut first_timestamp: Option<chrono::DateTime<Utc>> = None;

        for line in reader.lines().map_while(Result::ok) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };

            let entry_type = val.get("type").and_then(|v| v.as_str()).unwrap_or("");

            match entry_type {
                "runtime-config" => {
                    session_id = val
                        .get("sessionId")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    model = val
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    if first_timestamp.is_none() {
                        first_timestamp = Some(parse_timestamp(&val));
                    }
                }
                "user" => {
                    // Fallback: extract session_id from any entry if runtime-config is absent.
                    if session_id.is_none() {
                        session_id = val
                            .get("sessionId")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                    }
                    if model.is_none() {
                        model = val
                            .get("model")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                    }
                    if let Some(message) = val.get("message") {
                        let role = message
                            .get("role")
                            .and_then(|v| v.as_str())
                            .unwrap_or("user");
                        if role == "user" {
                            let content = message.get("content");
                            match content {
                                Some(serde_json::Value::String(text)) => {
                                    if cwd.is_none() {
                                        cwd = val
                                            .get("cwd")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string());
                                    }
                                    if git_branch.is_none() {
                                        git_branch = val
                                            .get("gitBranch")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string());
                                    }
                                    let timestamp = parse_timestamp(&val);
                                    if !current_user_content.is_empty() {
                                        turns.push(MessageTurn {
                                            id: format!("user-{}", turns.len()),
                                            role: TurnRole::User,
                                            blocks: std::mem::take(&mut current_user_content),
                                            timestamp,
                                            usage: None,
                                            duration_ms: None,
                                            model: None,
                                            completed_at: None,
                                        });
                                    }
                                    current_user_content.push(ContentBlock::Text {
                                        text: text.clone(),
                                    });
                                }
                                Some(serde_json::Value::Array(blocks)) => {
                                    for block in blocks {
                                        if block
                                            .get("type")
                                            .and_then(|v| v.as_str())
                                            == Some("tool_result")
                                        {
                                            let tool_use_id = block
                                                .get("tool_use_id")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .to_string();
                                            let output = block
                                                .get("content")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .to_string();
                                            let is_error = block
                                                .get("is_error")
                                                .and_then(|v| v.as_bool())
                                                .unwrap_or(false);
                                            current_user_content.push(
                                                ContentBlock::ToolResult {
                                                    tool_use_id: Some(tool_use_id),
                                                    output_preview: Some(output),
                                                    is_error,
                                                    agent_stats: None,
                                                },
                                            );
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                "assistant" => {
                    // Fallback: extract session_id from any entry if runtime-config is absent.
                    if session_id.is_none() {
                        session_id = val
                            .get("sessionId")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                    }
                    if model.is_none() {
                        model = val
                            .get("model")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                    }
                    if let Some(message) = val.get("message") {
                        if cwd.is_none() {
                            cwd = val
                                .get("cwd")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                        }
                        if git_branch.is_none() {
                            git_branch = val
                                .get("gitBranch")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                        }
                        let content_blocks = message.get("content");
                        if let Some(serde_json::Value::Array(blocks)) = content_blocks {
                            let timestamp = parse_timestamp(&val);

                            // Flush previous user content
                            if !current_user_content.is_empty() {
                                turns.push(MessageTurn {
                                    id: format!("user-{}", turns.len()),
                                    role: TurnRole::User,
                                    blocks: std::mem::take(&mut current_user_content),
                                    timestamp,
                                    usage: None,
                                    duration_ms: None,
                                    model: None,
                                    completed_at: None,
                                });
                            }

                            let mut assistant_blocks: Vec<ContentBlock> = Vec::new();
                            for block in blocks {
                                if let Some(block_type) =
                                    block.get("type").and_then(|v| v.as_str())
                                {
                                    match block_type {
                                        "thinking" => {
                                            if let Some(text) =
                                                block.get("thinking").and_then(|v| v.as_str())
                                            {
                                                assistant_blocks.push(ContentBlock::Thinking {
                                                    text: text.to_string(),
                                                });
                                            }
                                        }
                                        "text" => {
                                            if let Some(text) =
                                                block.get("text").and_then(|v| v.as_str())
                                            {
                                                assistant_blocks.push(ContentBlock::Text {
                                                    text: text.to_string(),
                                                });
                                            }
                                        }
                                        "tool_use" => {
                                            let tool_use_id = block
                                                .get("id")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .to_string();
                                            let tool_name = block
                                                .get("name")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .to_string();
                                            let input_preview = block
                                                .get("input")
                                                .map(|v| {
                                                    serde_json::to_string(v).unwrap_or_default()
                                                });
                                            assistant_blocks.push(ContentBlock::ToolUse {
                                                tool_use_id: Some(tool_use_id),
                                                tool_name,
                                                input_preview,
                                                meta: None,
                                            });
                                        }
                                        _ => {}
                                    }
                                }
                            }

                            if !assistant_blocks.is_empty() {
                                let stop_reason = message.get("stop_reason").and_then(|v| v.as_str());
                                let model_str = message
                                    .get("model")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());
                                let duration =
                                    if stop_reason == Some("end_turn") {
                                        val.get("duration_ms").and_then(|v| v.as_u64())
                                    } else {
                                        None
                                    };
                                let completed_at =
                                    if stop_reason.is_some() {
                                        Some(timestamp)
                                    } else {
                                        None
                                    };

                                turns.push(MessageTurn {
                                    id: format!("assistant-{}", turns.len()),
                                    role: TurnRole::Assistant,
                                    blocks: assistant_blocks,
                                    timestamp,
                                    usage: None,
                                    duration_ms: duration,
                                    model: model_str,
                                    completed_at,
                                });
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Flush remaining user content
        if !current_user_content.is_empty() {
            turns.push(MessageTurn {
                id: format!("user-{}", turns.len()),
                role: TurnRole::User,
                blocks: current_user_content,
                timestamp: Utc::now(),
                usage: None,
                duration_ms: None,
                model: None,
                completed_at: None,
            });
        }

        let sid = session_id?;
        // Use the actual cwd from JSONL entries as the folder_path.
        // The encoded directory name in ~/.qoder/projects/ is lossy for paths
        // containing dashes, so we cannot reliably decode it back.
        let folder_path = cwd.unwrap_or_else(|| {
            path.parent()
                .and_then(|p| p.to_str())
                .unwrap_or(".")
                .to_string()
        });
        let folder_name = folder_name_from_path(&folder_path);

        let title = turns
            .iter()
            .find(|t| t.role == TurnRole::User)
            .and_then(|t| t.blocks.first())
            .and_then(|b| match b {
                ContentBlock::Text { text } => Some(title_from_user_text(text)),
                _ => None,
            });

        let summary = ConversationSummary {
            id: sid.clone(),
            agent_type: AgentType::QoderCli,
            folder_path: Some(folder_path),
            folder_name: Some(folder_name),
            title,
            started_at: first_timestamp.unwrap_or_else(Utc::now),
            ended_at: turns.last().and_then(|t| t.completed_at),
            message_count: turns.len() as u32,
            model,
            git_branch,
            parent_id: None,
            parent_tool_use_id: None,
            delegation_call_id: None,
        };

        Some((summary, turns))
    }
}

impl AgentParser for QoderCliParser {
    fn list_conversations(&self) -> Result<Vec<ConversationSummary>, ParseError> {
        let files = self.list_jsonl_files();
        let mut summaries = Vec::new();

        for path in &files {
            if let Some((summary, _turns)) = self.parse_jsonl_file(path) {
                summaries.push(summary);
            }
        }

        summaries.sort_by_key(|b| std::cmp::Reverse(b.started_at));
        Ok(summaries)
    }

    fn get_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<ConversationDetail, ParseError> {
        let files = self.list_jsonl_files();

        for path in &files {
            if let Some((summary, turns)) = self.parse_jsonl_file(path) {
                if summary.id == conversation_id {
                    return Ok(ConversationDetail {
                        summary,
                        turns,
                        session_stats: None,
                    });
                }
            }
        }

        Err(ParseError::ConversationNotFound(
            conversation_id.to_string(),
        ))
    }
}

fn parse_timestamp(val: &serde_json::Value) -> chrono::DateTime<Utc> {
    val.get("timestamp")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(Utc::now)
}

fn resolve_qodercli_base_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".qoder")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_jsonl(entries: &[&str]) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new()
            .suffix(".jsonl")
            .tempfile()
            .unwrap();
        for entry in entries {
            writeln!(f, "{entry}").unwrap();
        }
        f
    }

    #[test]
    fn parse_session_with_runtime_config() {
        let f = make_jsonl(&[
            r#"{"type":"runtime-config","sessionId":"sid-123","model":"qmodel_latest","timestamp":"2026-01-01T00:00:00Z"}"#,
            r#"{"type":"user","timestamp":"2026-01-01T00:00:01Z","message":{"role":"user","content":"hi"},"cwd":"D:\\test","sessionId":"sid-123","gitBranch":"main"}"#,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:02Z","message":{"role":"assistant","model":"qmodel_latest","stop_reason":"end_turn","content":[{"type":"text","text":"hello"}]},"cwd":"D:\\test","sessionId":"sid-123"}"#,
        ]);
        let parser = QoderCliParser::new();
        let (summary, turns) = parser.parse_jsonl_file(f.path()).unwrap();
        assert_eq!(summary.id, "sid-123");
        assert_eq!(summary.model.as_deref(), Some("qmodel_latest"));
        assert_eq!(summary.folder_path.as_deref(), Some("D:\\test"));
        assert_eq!(summary.git_branch.as_deref(), Some("main"));
        assert!(!turns.is_empty());
    }

    #[test]
    fn parse_session_without_runtime_config() {
        // Real QoderCli format: no runtime-config entry, sessionId on user/assistant entries.
        let f = make_jsonl(&[
            r#"{"type":"user","uuid":"u1","timestamp":"2026-06-14T10:39:52.062Z","message":{"role":"user","content":"hello"},"cwd":"D:\\xiaogou\\codeg","sessionId":"b49bc65b","gitBranch":"main"}"#,
            r#"{"type":"assistant","uuid":"a1","timestamp":"2026-06-14T10:39:57.206Z","message":{"id":"c1","type":"message","role":"assistant","model":"qmodel_latest","stop_reason":"end_turn","content":[{"type":"text","text":"Hello!"}]},"cwd":"D:\\xiaogou\\codeg","sessionId":"b49bc65b"}"#,
            r#"{"type":"last-prompt","sessionId":"b49bc65b","lastPrompt":"hello"}"#,
        ]);
        let parser = QoderCliParser::new();
        let result = parser.parse_jsonl_file(f.path());
        assert!(result.is_some(), "parser must not return None when runtime-config is absent");
        let (summary, turns) = result.unwrap();
        assert_eq!(summary.id, "b49bc65b");
        assert_eq!(summary.folder_path.as_deref(), Some("D:\\xiaogou\\codeg"));
        assert_eq!(summary.git_branch.as_deref(), Some("main"));
        assert!(!turns.is_empty(), "must have parsed turns");
    }

    #[test]
    fn parse_session_without_runtime_config_has_correct_turns() {
        let f = make_jsonl(&[
            r#"{"type":"user","timestamp":"2026-01-01T00:00:01Z","message":{"role":"user","content":"ping"},"cwd":"C:\\proj","sessionId":"s1"}"#,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:02Z","message":{"id":"a1","type":"message","role":"assistant","model":"m","stop_reason":"end_turn","content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"pong"}]},"cwd":"C:\\proj","sessionId":"s1"}"#,
        ]);
        let parser = QoderCliParser::new();
        let (_, turns) = parser.parse_jsonl_file(f.path()).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, TurnRole::User);
        assert_eq!(turns[1].role, TurnRole::Assistant);
        // Assistant turn should have thinking + text blocks
        assert_eq!(turns[1].blocks.len(), 2);
    }

    #[test]
    fn parse_empty_session_returns_none() {
        let f = make_jsonl(&[]);
        let parser = QoderCliParser::new();
        assert!(parser.parse_jsonl_file(f.path()).is_none());
    }

    #[test]
    fn parse_session_only_last_prompt_returns_none() {
        let f = make_jsonl(&[
            r#"{"type":"last-prompt","sessionId":"s1","lastPrompt":"hi"}"#,
        ]);
        let parser = QoderCliParser::new();
        assert!(parser.parse_jsonl_file(f.path()).is_none());
    }
}
