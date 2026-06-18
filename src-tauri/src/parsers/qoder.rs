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
    ) -> Option<(ConversationSummary, Vec<MessageTurn>, Option<u64>)> {
        let file = fs::File::open(path).ok()?;
        let reader = BufReader::new(file);

        let mut session_id: Option<String> = None;
        let mut model: Option<String> = None;
        let mut turns: Vec<MessageTurn> = Vec::new();
        let mut current_user_content: Vec<ContentBlock> = Vec::new();
        let mut cwd: Option<String> = None;
        let mut git_branch: Option<String> = None;
        let mut first_timestamp: Option<chrono::DateTime<Utc>> = None;
        let mut context_window_max_tokens: Option<u64> = None;

        // Buffer for merging assistant entries that share the same message.id.
        // QoderCli writes each content block (thinking, tool_use, text) as a
        // separate JSONL entry for the same assistant message, so we must
        // accumulate them and flush as one turn when the id changes.
        let mut assistant_msg_id: Option<String> = None;
        let mut assistant_blocks: Vec<ContentBlock> = Vec::new();
        let mut assistant_timestamp: Option<chrono::DateTime<Utc>> = None;
        let mut assistant_stop_reason: Option<String> = None;
        let mut assistant_model: Option<String> = None;
        let mut assistant_duration_ms: Option<u64> = None;

        // Flush the accumulated assistant buffer as a single turn.
        let flush_assistant = |turns: &mut Vec<MessageTurn>,
                               assistant_msg_id: &mut Option<String>,
                               assistant_blocks: &mut Vec<ContentBlock>,
                               assistant_timestamp: &mut Option<chrono::DateTime<Utc>>,
                               assistant_stop_reason: &mut Option<String>,
                               assistant_model: &mut Option<String>,
                               assistant_duration_ms: &mut Option<u64>| {
            if assistant_blocks.is_empty() {
                *assistant_msg_id = None;
                return;
            }
            let blocks = std::mem::take(assistant_blocks);
            let ts = assistant_timestamp.unwrap_or_else(Utc::now);
            let completed_at = if assistant_stop_reason.is_some() {
                Some(ts)
            } else {
                None
            };
            let dur = if assistant_stop_reason.as_deref() == Some("end_turn") {
                *assistant_duration_ms
            } else {
                None
            };
            turns.push(MessageTurn {
                id: format!("assistant-{}", turns.len()),
                role: TurnRole::Assistant,
                blocks,
                timestamp: ts,
                usage: None,
                duration_ms: dur,
                model: assistant_model.clone(),
                completed_at,
            });
            *assistant_msg_id = None;
            *assistant_timestamp = None;
            *assistant_stop_reason = None;
            *assistant_model = None;
            *assistant_duration_ms = None;
        };

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
                    if let Some(cw) = val.get("contextWindow").and_then(|v| v.as_u64()) {
                        if cw > 0 {
                            context_window_max_tokens = Some(cw);
                        }
                    }
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
                                    // Flush pending assistant buffer before user content
                                    flush_assistant(
                                        &mut turns,
                                        &mut assistant_msg_id,
                                        &mut assistant_blocks,
                                        &mut assistant_timestamp,
                                        &mut assistant_stop_reason,
                                        &mut assistant_model,
                                        &mut assistant_duration_ms,
                                    );
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

                                    // Check if ALL blocks are tool_result type.
                                    // When a user entry contains only tool_results
                                    // (the MCP tool result payload for a preceding
                                    // tool_use), merge them into the last assistant
                                    // turn so the adapter can match tool_use ↔
                                    // tool_result within the same turn.
                                    let all_tool_results = !blocks.is_empty()
                                        && blocks.iter().all(|b| {
                                            b.get("type")
                                                .and_then(|v| v.as_str())
                                                == Some("tool_result")
                                        });

                                    if all_tool_results {
                                        // Extract tool_result blocks and attach them
                                        // to the most recent assistant turn in `turns`.
                                        // Flush any pending assistant buffer first so
                                        // the target turn is already in `turns`.
                                        flush_assistant(
                                            &mut turns,
                                            &mut assistant_msg_id,
                                            &mut assistant_blocks,
                                            &mut assistant_timestamp,
                                            &mut assistant_stop_reason,
                                            &mut assistant_model,
                                            &mut assistant_duration_ms,
                                        );
                                        // Find the last assistant turn and append the
                                        // tool_result blocks to it.
                                        if let Some(last_turn) =
                                            turns.iter_mut().rev().find(|t| {
                                                matches!(t.role, TurnRole::Assistant)
                                            })
                                        {
                                            for block in blocks {
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
                                                last_turn.blocks.push(
                                                    ContentBlock::ToolResult {
                                                        tool_use_id: Some(
                                                            tool_use_id,
                                                        ),
                                                        output_preview: Some(
                                                            output,
                                                        ),
                                                        is_error,
                                                        agent_stats: None,
                                                    },
                                                );
                                            }
                                        } else {
                                            // No assistant turn to merge into —
                                            // fall back to creating a normal user turn.
                                            flush_assistant(
                                                &mut turns,
                                                &mut assistant_msg_id,
                                                &mut assistant_blocks,
                                                &mut assistant_timestamp,
                                                &mut assistant_stop_reason,
                                                &mut assistant_model,
                                                &mut assistant_duration_ms,
                                            );
                                            if !current_user_content.is_empty()
                                            {
                                                turns.push(MessageTurn {
                                                    id: format!(
                                                        "user-{}",
                                                        turns.len()
                                                    ),
                                                    role: TurnRole::User,
                                                    blocks: std::mem::take(
                                                        &mut current_user_content,
                                                    ),
                                                    timestamp,
                                                    usage: None,
                                                    duration_ms: None,
                                                    model: None,
                                                    completed_at: None,
                                                });
                                            }
                                            for block in blocks {
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
                                                        tool_use_id: Some(
                                                            tool_use_id,
                                                        ),
                                                        output_preview: Some(
                                                            output,
                                                        ),
                                                        is_error,
                                                        agent_stats: None,
                                                    },
                                                );
                                            }
                                        }
                                    } else {
                                        // Mixed content (text + tool_result, or
                                        // just text) — existing logic: flush and
                                        // create a normal user turn.
                                        flush_assistant(
                                            &mut turns,
                                            &mut assistant_msg_id,
                                            &mut assistant_blocks,
                                            &mut assistant_timestamp,
                                            &mut assistant_stop_reason,
                                            &mut assistant_model,
                                            &mut assistant_duration_ms,
                                        );
                                        if !current_user_content.is_empty() {
                                            turns.push(MessageTurn {
                                                id: format!(
                                                    "user-{}",
                                                    turns.len()
                                                ),
                                                role: TurnRole::User,
                                                blocks: std::mem::take(
                                                    &mut current_user_content,
                                                ),
                                                timestamp,
                                                usage: None,
                                                duration_ms: None,
                                                model: None,
                                                completed_at: None,
                                            });
                                        }
                                        for block in blocks {
                                            let block_type = block
                                                .get("type")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("");
                                            match block_type {
                                                "text" => {
                                                    if let Some(text) = block
                                                        .get("text")
                                                        .and_then(|v| v.as_str())
                                                    {
                                                        current_user_content.push(
                                                            ContentBlock::Text {
                                                                text: text
                                                                    .to_string(),
                                                            },
                                                        );
                                                    }
                                                }
                                                "tool_result" => {
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
                                                            tool_use_id: Some(
                                                                tool_use_id,
                                                            ),
                                                            output_preview: Some(
                                                                output,
                                                            ),
                                                            is_error,
                                                            agent_stats: None,
                                                        },
                                                    );
                                                }
                                                _ => {}
                                            }
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

                        let msg_id = message
                            .get("id")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());

                        // Check if this is a different assistant message than the buffered one.
                        // If so, flush the previous buffer as a complete turn.
                        if assistant_msg_id.is_some()
                            && assistant_msg_id.as_deref() != msg_id.as_deref()
                        {
                            // Flush accumulated user content first
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
                            flush_assistant(
                                &mut turns,
                                &mut assistant_msg_id,
                                &mut assistant_blocks,
                                &mut assistant_timestamp,
                                &mut assistant_stop_reason,
                                &mut assistant_model,
                                &mut assistant_duration_ms,
                            );
                        }

                        // Set or update the assistant message buffer
                        if assistant_msg_id.is_none() {
                            // New assistant message — flush any pending user content
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
                            assistant_msg_id = msg_id.clone();
                        }

                        // Accumulate content blocks from this entry
                        if let Some(serde_json::Value::Array(blocks)) = message.get("content") {
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
                        }

                        // Update metadata from the latest entry (later entries have stop_reason)
                        if let Some(sr) = message.get("stop_reason").and_then(|v| v.as_str()) {
                            assistant_stop_reason = Some(sr.to_string());
                        }
                        if let Some(m) = message.get("model").and_then(|v| v.as_str()) {
                            assistant_model = Some(m.to_string());
                        }
                        if assistant_stop_reason.as_deref() == Some("end_turn") {
                            assistant_duration_ms = val
                                .get("duration_ms")
                                .and_then(|v| v.as_u64());
                        }
                        // Always update timestamp to the latest entry
                        assistant_timestamp = Some(parse_timestamp(&val));
                    }
                }
                _ => {}
            }
        }

        // Flush remaining assistant buffer
        flush_assistant(
            &mut turns,
            &mut assistant_msg_id,
            &mut assistant_blocks,
            &mut assistant_timestamp,
            &mut assistant_stop_reason,
            &mut assistant_model,
            &mut assistant_duration_ms,
        );

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

        Some((summary, turns, context_window_max_tokens))
    }
}

impl AgentParser for QoderCliParser {
    fn list_conversations(&self) -> Result<Vec<ConversationSummary>, ParseError> {
        let files = self.list_jsonl_files();
        let mut summaries = Vec::new();

        for path in &files {
            if let Some((summary, _turns, _ctx_max)) = self.parse_jsonl_file(path) {
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
            if let Some((summary, turns, ctx_max)) = self.parse_jsonl_file(path) {
                if summary.id == conversation_id {
                    let mut stats = super::compute_session_stats(&turns);
                    if let Some(ref mut s) = stats {
                        s.context_window_max_tokens = ctx_max.or(s.context_window_max_tokens);
                        if let (Some(used), Some(max)) =
                            (s.total_tokens, s.context_window_max_tokens)
                        {
                            s.context_window_used_tokens = Some(used);
                            if max > 0 {
                                s.context_window_usage_percent =
                                    Some((used as f64 / max as f64) * 100.0);
                            }
                        }
                    }
                    return Ok(ConversationDetail {
                        summary,
                        turns,
                        session_stats: stats,
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
        let (summary, turns, _ctx_max) = parser.parse_jsonl_file(f.path()).unwrap();
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
        let (summary, turns, _) = result.unwrap();
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
        let (_, turns, _) = parser.parse_jsonl_file(f.path()).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, TurnRole::User);
        assert_eq!(turns[1].role, TurnRole::Assistant);
        // Assistant turn should have thinking + text blocks
        assert_eq!(turns[1].blocks.len(), 2);
    }

    #[test]
    fn merge_assistant_entries_with_same_message_id() {
        // QoderCli writes each content block as a separate JSONL entry for the
        // same assistant message (same message.id). These must be merged into
        // a single turn, not split into multiple turns.
        let f = make_jsonl(&[
            r#"{"type":"user","timestamp":"2026-01-01T00:00:01Z","message":{"role":"user","content":"hi"},"cwd":"C:\\proj","sessionId":"s1"}"#,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:02Z","message":{"id":"m1","type":"message","role":"assistant","model":"qmodel","stop_reason":null,"content":[{"type":"thinking","thinking":"thinking..."}]},"cwd":"C:\\proj","sessionId":"s1"}"#,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:03Z","message":{"id":"m1","type":"message","role":"assistant","model":"qmodel","stop_reason":"end_turn","content":[{"type":"text","text":"hello!"}]},"cwd":"C:\\proj","sessionId":"s1"}"#,
        ]);
        let parser = QoderCliParser::new();
        let (_, turns, _) = parser.parse_jsonl_file(f.path()).unwrap();
        // Should be 2 turns: user + assistant (merged), NOT 3 turns
        assert_eq!(turns.len(), 2, "assistant entries with same message.id must merge into one turn");
        assert_eq!(turns[0].role, TurnRole::User);
        assert_eq!(turns[1].role, TurnRole::Assistant);
        // The merged assistant turn should have both thinking + text blocks
        assert_eq!(turns[1].blocks.len(), 2, "merged turn should have thinking + text blocks");
        assert!(matches!(turns[1].blocks[0], ContentBlock::Thinking { .. }));
        assert!(matches!(turns[1].blocks[1], ContentBlock::Text { .. }));
        // Should have completed_at from the last entry (stop_reason: end_turn)
        assert!(turns[1].completed_at.is_some(), "merged turn should have completed_at");
    }

    #[test]
    fn separate_assistant_entries_with_different_ids() {
        // Assistant entries with different message.id should create separate turns.
        let f = make_jsonl(&[
            r#"{"type":"user","timestamp":"2026-01-01T00:00:01Z","message":{"role":"user","content":"hi"},"cwd":"C:\\proj","sessionId":"s1"}"#,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:02Z","message":{"id":"m1","type":"message","role":"assistant","model":"qmodel","stop_reason":"end_turn","content":[{"type":"text","text":"reply 1"}]},"cwd":"C:\\proj","sessionId":"s1"}"#,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:03Z","message":{"id":"m2","type":"message","role":"assistant","model":"qmodel","stop_reason":"end_turn","content":[{"type":"text","text":"reply 2"}]},"cwd":"C:\\proj","sessionId":"s1"}"#,
        ]);
        let parser = QoderCliParser::new();
        let (_, turns, _) = parser.parse_jsonl_file(f.path()).unwrap();
        // Should be 3 turns: user + assistant(m1) + assistant(m2)
        assert_eq!(turns.len(), 3, "different message.id should create separate turns");
        assert_eq!(turns[0].role, TurnRole::User);
        assert_eq!(turns[1].role, TurnRole::Assistant);
        assert_eq!(turns[2].role, TurnRole::Assistant);
    }

    #[test]
    fn thinking_only_entry_buffers_until_text_arrives() {
        // A thinking-only entry (stop_reason: null) should be buffered,
        // and the text entry (stop_reason: end_turn) completes the turn.
        let f = make_jsonl(&[
            r#"{"type":"user","timestamp":"2026-01-01T00:00:01Z","message":{"role":"user","content":"ping"},"cwd":"C:\\proj","sessionId":"s1"}"#,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:02Z","message":{"id":"m1","type":"message","role":"assistant","model":"qmodel","stop_reason":null,"content":[{"type":"thinking","thinking":"hmm"}]},"cwd":"C:\\proj","sessionId":"s1"}"#,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:03Z","message":{"id":"m1","type":"message","role":"assistant","model":"qmodel","stop_reason":"end_turn","content":[{"type":"text","text":"pong"}]},"cwd":"C:\\proj","sessionId":"s1"}"#,
            r#"{"type":"user","timestamp":"2026-01-01T00:00:04Z","message":{"role":"user","content":"ping2"},"cwd":"C:\\proj","sessionId":"s1"}"#,
        ]);
        let parser = QoderCliParser::new();
        let (_, turns, _) = parser.parse_jsonl_file(f.path()).unwrap();
        // user(1) + merged assistant(m1) + user(2) = 3 turns
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].role, TurnRole::User);
        assert_eq!(turns[1].role, TurnRole::Assistant);
        assert_eq!(turns[2].role, TurnRole::User);
        assert_eq!(turns[1].blocks.len(), 2, "merged thinking + text");
    }

    #[test]
    fn tool_use_entry_merges_with_thinking() {
        // tool_use entry with same message.id as thinking entry should merge.
        let f = make_jsonl(&[
            r#"{"type":"user","timestamp":"2026-01-01T00:00:01Z","message":{"role":"user","content":"write file"},"cwd":"C:\\proj","sessionId":"s1"}"#,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:02Z","message":{"id":"m1","type":"message","role":"assistant","model":"qmodel","stop_reason":null,"content":[{"type":"thinking","thinking":"need to write"}]},"cwd":"C:\\proj","sessionId":"s1"}"#,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:03Z","message":{"id":"m1","type":"message","role":"assistant","model":"qmodel","stop_reason":"tool_use","content":[{"type":"tool_use","id":"t1","name":"Write","input":{"path":"/tmp/x"}}]},"cwd":"C:\\proj","sessionId":"s1"}"#,
            r#"{"type":"user","timestamp":"2026-01-01T00:00:04Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]},"cwd":"C:\\proj","sessionId":"s1"}"#,
        ]);
        let parser = QoderCliParser::new();
        let (_, turns, _) = parser.parse_jsonl_file(f.path()).unwrap();
        // user(1) + merged assistant(thinking+tool_use+tool_result) = 2 turns
        // tool_result-only user turns merge into the previous assistant turn
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, TurnRole::User);
        assert_eq!(turns[1].role, TurnRole::Assistant);
        assert_eq!(turns[1].blocks.len(), 3, "merged thinking + tool_use + tool_result");
        assert!(matches!(turns[1].blocks[0], ContentBlock::Thinking { .. }));
        assert!(matches!(turns[1].blocks[1], ContentBlock::ToolUse { .. }));
        assert!(matches!(turns[1].blocks[2], ContentBlock::ToolResult { .. }));
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

    #[test]
    fn parse_user_content_as_array_of_text_blocks() {
        let f = make_jsonl(&[
            r#"{"type":"user","uuid":"u1","timestamp":"2026-06-17T13:20:54.451Z","message":{"role":"user","content":[{"type":"text","text":"hello"}]},"cwd":"D:\\test","sessionId":"s1"}"#,
            r#"{"type":"assistant","uuid":"a1","timestamp":"2026-06-17T13:20:58.413Z","message":{"id":"m1","type":"message","role":"assistant","model":"qmodel","stop_reason":"end_turn","content":[{"type":"text","text":"hi there"}]},"cwd":"D:\\test","sessionId":"s1"}"#,
        ]);
        let parser = QoderCliParser::new();
        let (_, turns, _) = parser.parse_jsonl_file(f.path()).unwrap();
        assert_eq!(turns.len(), 2, "must parse user turn from array-format content");
        assert_eq!(turns[0].role, TurnRole::User);
        assert_eq!(turns[1].role, TurnRole::Assistant);
        // Verify the user text block was captured
        match &turns[0].blocks[0] {
            ContentBlock::Text { text } => assert_eq!(text, "hello"),
            other => panic!("expected Text block, got {:?}", other),
        }
    }

    #[test]
    fn parse_user_content_array_with_multiple_blocks() {
        let f = make_jsonl(&[
            r#"{"type":"user","uuid":"u1","timestamp":"2026-06-17T13:20:54.451Z","message":{"role":"user","content":[{"type":"text","text":"ping"},{"type":"text","text":"pong"}]},"cwd":"D:\\test","sessionId":"s1"}"#,
            r#"{"type":"assistant","uuid":"a1","timestamp":"2026-06-17T13:20:58.413Z","message":{"id":"m1","type":"message","role":"assistant","model":"qmodel","stop_reason":"end_turn","content":[{"type":"text","text":"response"}]},"cwd":"D:\\test","sessionId":"s1"}"#,
        ]);
        let parser = QoderCliParser::new();
        let (_, turns, _) = parser.parse_jsonl_file(f.path()).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].blocks.len(), 2, "must capture both text blocks from array");
    }

    #[test]
    fn tool_result_only_user_turn_merges_into_assistant() {
        // QoderCli stores tool_result for ask_user_question in a user turn.
        // The tool_result-only user turn should merge into the previous
        // assistant turn so the adapter can match tool_use ↔ tool_result.
        let f = make_jsonl(&[
            r#"{"type":"user","uuid":"u1","timestamp":"2026-06-16T15:14:27Z","message":{"role":"user","content":[{"type":"text","text":"ask a question"}]},"cwd":"D:\\test","sessionId":"s1"}"#,
            r#"{"type":"assistant","uuid":"a1","timestamp":"2026-06-16T15:14:32Z","message":{"id":"m1","type":"message","role":"assistant","model":"qmodel","stop_reason":null,"content":[{"type":"thinking","thinking":"hmm"}]},"cwd":"D:\\test","sessionId":"s1"}"#,
            r#"{"type":"assistant","uuid":"a2","timestamp":"2026-06-16T15:14:32Z","message":{"id":"m1","type":"message","role":"assistant","model":"qmodel","stop_reason":"tool_use","content":[{"type":"tool_use","id":"call_abc","name":"mcp__codeg-mcp__ask_user_question","input":{"questions":[{"header":"Fruit","multiSelect":false,"options":[{"label":"Apple","description":""},{"label":"Banana","description":""}],"question":"What fruit?"}]}}]},"cwd":"D:\\test","sessionId":"s1"}"#,
            r#"{"type":"user","uuid":"u2","timestamp":"2026-06-16T15:15:00Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_abc","content":"The user answered your question(s):\n1. [Fruit] What fruit?\n   → Apple\n","is_error":false}]},"cwd":"D:\\test","sessionId":"s1"}"#,
            r#"{"type":"assistant","uuid":"a3","timestamp":"2026-06-16T15:15:03Z","message":{"id":"m2","type":"message","role":"assistant","model":"qmodel","stop_reason":"end_turn","content":[{"type":"text","text":"You chose Apple!"}]},"cwd":"D:\\test","sessionId":"s1"}"#,
        ]);
        let parser = QoderCliParser::new();
        let (_, turns, _) = parser.parse_jsonl_file(f.path()).unwrap();
        // user(1) + assistant(m1 with tool_result merged) + assistant(m2) = 3 turns
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].role, TurnRole::User);
        assert_eq!(turns[1].role, TurnRole::Assistant);
        assert_eq!(turns[2].role, TurnRole::Assistant);
        // The first assistant turn should have thinking + tool_use + tool_result
        assert_eq!(turns[1].blocks.len(), 3, "thinking + tool_use + tool_result");
        assert!(matches!(turns[1].blocks[0], ContentBlock::Thinking { .. }));
        assert!(matches!(turns[1].blocks[1], ContentBlock::ToolUse { .. }));
        assert!(matches!(turns[1].blocks[2], ContentBlock::ToolResult { .. }));
        // Verify the tool_result output is correct
        if let ContentBlock::ToolResult { tool_use_id, output_preview, .. } = &turns[1].blocks[2] {
            assert_eq!(tool_use_id.as_deref(), Some("call_abc"));
            assert!(output_preview.as_deref().unwrap().contains("Apple"));
        } else {
            panic!("expected ToolResult block");
        }
    }
}
