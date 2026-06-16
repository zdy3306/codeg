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
        let mut _context_window: Option<u64> = None;
        let mut turns: Vec<MessageTurn> = Vec::new();
        let mut current_user_content: Vec<ContentBlock> = Vec::new();
        let mut _cwd: Option<String> = None;
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
                    _context_window = val.get("contextWindow").and_then(|v| v.as_u64());
                    if first_timestamp.is_none() {
                        first_timestamp = Some(parse_timestamp(&val));
                    }
                }
                "user" => {
                    if let Some(message) = val.get("message") {
                        let role = message
                            .get("role")
                            .and_then(|v| v.as_str())
                            .unwrap_or("user");
                        if role == "user" {
                            let content = message.get("content");
                            match content {
                                Some(serde_json::Value::String(text)) => {
                                    _cwd = val
                                        .get("cwd")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string());
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
                    if let Some(message) = val.get("message") {
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
        // Decode the encoded directory name back to the actual workspace path.
        // QoderCli encodes "D:\codeg" as "D--codeg" (replacing ':' and '\' with '-').
        let encoded_dir = path.parent()?.file_name()?.to_string_lossy().to_string();
        let decoded_path = decode_qodercli_folder_path(&encoded_dir);
        let folder_path = decoded_path.unwrap_or_else(|| {
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
            git_branch: None,
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

/// Decode QoderCli's encoded folder path back to the original workspace path.
/// QoderCli encodes "D:\codeg" as "D--codeg" (replacing ':' and '\' with '-').
/// On Windows: "D--codeg" → "D:\codeg", "C--Users-zdy33-proj" → "C:\Users\zdy33\proj"
/// On Unix: "/home/user/project" stays as-is (no encoding needed).
fn decode_qodercli_folder_path(encoded: &str) -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        // Windows paths like "D--codeg" or "C--Users-zdy33-proj"
        // The first '-' after the drive letter represents ':', all others represent '\'
        let bytes = encoded.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b'-' {
            // Drive letter found, find the second '-' to split
            if let Some(pos) = encoded[2..].find('-') {
                let drive = &encoded[..1];
                let rest = &encoded[2 + pos + 1..];
                let decoded_rest = rest.replace('-', "\\");
                return Some(format!("{}:\\{}", drive, decoded_rest));
            }
        }
        // Fallback: treat as-is
        Some(encoded.to_string())
    }
    #[cfg(not(target_os = "windows"))]
    {
        // Unix paths are not encoded by QoderCli
        Some(encoded.replace('-', "/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_base_dir() {
        let parser = QoderCliParser::new();
        assert!(parser.base_dir.ends_with(".qoder"));
    }
}
