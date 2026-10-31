//! Rebuilding the snapshot Ante refuses to open.
//!
//! Ante writes `meta.json` and `dialog.json` when a turn *ends*, so a session
//! whose first turn was killed (crash, reboot, `pkill`, closed terminal) leaves
//! its `events.jsonl` and nothing else — and `ResumeSession` on that directory
//! answers `Failed to resume session: No such file or directory (os error 2)`.
//! The log is intact; only the two files that say "this conversation is
//! resumable" are missing.
//!
//! Measured against Ante 0.3 (isolated `ANTE_HOME`, see the module's tests):
//!
//! * `meta.json` absent  → refused, `dialog.json` present or not.
//! * `dialog.json` absent → refused, `meta.json` present or not.
//! * both present        → resumes, whatever else is missing.
//!
//! So the repair is to write what is missing and nothing else. Keeping Ante's
//! own `dialog.json` when it is still there matters: it is the exact context the
//! model saw, and a file rebuilt from `events.jsonl` is a reconstruction — it
//! reproduces the conversation, not every field Ante's own writer would have.
//! The rebuild is the fallback for the case where both are gone.
//!
//! Writing into Ante's directory is deliberate and bounded: only the two files
//! Ante itself owns, only for a session already on disk, and only when one is
//! missing. `meta.json` is left alone when present, because Ante rewrites it
//! wholesale at `TurnEnd` and a hand-made one would be a regression.

use serde_json::{Value, json};

/// What a repair managed to do, for the log line.
#[derive(Debug, PartialEq, Eq)]
pub enum Repair {
    /// Nothing was missing; the archive was already resumable.
    Intact,
    /// `meta.json` was reconstructed around Ante's own `dialog.json`.
    MetaOnly,
    /// Both files were gone, so the conversation was rebuilt from the log.
    Rebuilt,
    /// The log is missing or empty — there is nothing to rebuild from.
    NoLog,
}

/// What the archive cannot say for itself.
///
/// A session's log records the conversation, not its setting: there is no
/// `SessionStart` in it (the first event of a real killed archive is the
/// `UserInput`), so the working directory and the provider/model pair come from
/// the caller. They are not cosmetic — Ante refuses a resume whose `dir` is
/// missing or gone (`Failed to resume session: session cwd does not exist: `),
/// which is exactly the failure a meta written from the log alone produces.
pub struct Context {
    /// The directory the session runs in.
    pub cwd: String,
    /// The pair to record when the log does not name one.
    pub provider: String,
    pub model: String,
}

/// Make `dir` resumable if `events.jsonl` says it can be, and report what it took.
///
/// `dir` is the session's own directory: `<ante home>/sessions/<id>`. The caller
/// has already decided the archive exists.
pub fn repair(dir: &std::path::Path, context: &Context) -> Repair {
    let log = dir.join("events.jsonl");
    let has_meta = dir.join("meta.json").is_file();
    let has_dialog = dir.join("dialog.json").is_file();
    if has_meta && has_dialog {
        return Repair::Intact;
    }
    let Ok(raw) = std::fs::read_to_string(&log) else {
        return Repair::NoLog;
    };
    let log = Log::parse(&raw);
    if log.user_inputs.is_empty() {
        return Repair::NoLog;
    }
    let id = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Ante's own snapshot is the better half: it is what the model actually saw,
    // including the environment notice, the system prompt and the token counts.
    // Only the summary around it is missing.
    if has_dialog {
        return if write_meta(dir, &log, &id, context).is_ok() {
            Repair::MetaOnly
        } else {
            Repair::NoLog
        };
    }
    let Some(dialog) = log.dialog(&id) else {
        return Repair::NoLog;
    };
    // The summary first: if the snapshot cannot be written afterwards, the
    // archive is no worse off than it was (Ante needs both to resume).
    if write_meta(dir, &log, &id, context).is_err() {
        return Repair::NoLog;
    }
    if write_json(&dir.join("dialog.json"), &dialog).is_err() {
        return Repair::NoLog;
    }
    Repair::Rebuilt
}

fn write_meta(dir: &std::path::Path, log: &Log, id: &str, context: &Context) -> Result<(), ()> {
    write_json(&dir.join("meta.json"), &log.meta(id, context))
}

/// Whole file, renamed into place: Ante reads these at startup, and a half-written
/// snapshot is a session that cannot be opened at all.
fn write_json(path: &std::path::Path, body: &Value) -> Result<(), ()> {
    let text = serde_json::to_string_pretty(body).map_err(|_| ())?;
    let temporary = path.with_extension("json.antex-tmp");
    std::fs::write(&temporary, text).map_err(|_| ())?;
    std::fs::rename(&temporary, path).map_err(|_| ())
}

/// The parts of a session's event log the repair needs.
struct Log {
    /// The first timestamp in the log, which is what the session started at.
    started: Option<String>,
    /// The last timestamp, for `last_active_time`.
    last: Option<String>,
    /// One entry per `UserInput`, in order.
    user_inputs: Vec<String>,
    /// The provider/model pair, when an event happens to name them. A real killed
    /// log starts at `UserInput` and carries neither, so the caller's fallback is
    /// the usual answer.
    provider: Option<String>,
    model: Option<String>,
    /// The conversation, as (role, content) pairs ready for `dialog.json`.
    messages: Vec<Value>,
}

impl Log {
    fn parse(raw: &str) -> Self {
        let mut log = Log {
            started: None,
            last: None,
            user_inputs: Vec::new(),
            provider: None,
            model: None,
            messages: Vec::new(),
        };
        // Tool calls by id are not needed: a `ToolEnd` names the call it answers,
        // and a result with no matching start is still a result.

        for line in raw.lines() {
            let Ok(wrapper) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let stamp = wrapper
                .get("timestamp")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            if log.started.is_none() {
                log.started.clone_from(&stamp);
            }
            if stamp.is_some() {
                log.last.clone_from(&stamp);
            }
            let Some(event) = wrapper.get("event").and_then(|e| e.as_object()) else {
                continue;
            };
            for (kind, payload) in event {
                match kind.as_str() {
                    "SessionStart" | "SessionUpdated" => {
                        if log.provider.is_none() {
                            log.provider = payload
                                .get("provider")
                                .and_then(|p| p.get("id"))
                                .and_then(|v| v.as_str())
                                .map(str::to_string);
                        }
                        if log.model.is_none() {
                            log.model = payload
                                .get("model")
                                .and_then(|m| m.get("id"))
                                .and_then(|v| v.as_str())
                                .map(str::to_string);
                        }
                    }
                    "TurnStart" => {}
                    "UserInput" => {
                        let Some(text) = payload.as_str() else {
                            continue;
                        };
                        log.user_inputs.push(text.to_string());
                        // Ante's own log keeps the staged mentions and its
                        // expansion of them; the conversation wants the prompt.
                        let shown = text.split("\n\n<folder-structure>").next().unwrap_or(text);
                        log.messages
                            .push(json!({ "role": "User", "content": { "Text": shown } }));
                    }
                    "Thinking" => {
                        let Some(text) = payload.as_str() else {
                            continue;
                        };
                        log.model_part(json!({ "Thinking": { "text": text, "signature": null } }));
                    }
                    "AgentMessage" => {
                        let Some(text) = payload.as_str() else {
                            continue;
                        };
                        // A killed turn is closed in the log with this marker;
                        // it is not something the model ever said.
                        if text.trim() == "...[interrupted]" {
                            continue;
                        }
                        log.model_part(json!({ "Text": text }));
                    }
                    "ToolStart" => {
                        if let Some(id) = payload.get("id").and_then(|v| v.as_str()) {
                            let name = payload
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default();
                            log.model_part(json!({ "ToolUse": {
                                "id": id,
                                "name": name,
                                "args": payload.get("args").cloned().unwrap_or_else(|| json!({})),
                            }}));
                        }
                    }
                    "ToolEnd" => {
                        let Some(call) = payload.get("tool_use_id").and_then(|v| v.as_str()) else {
                            continue;
                        };
                        let (text, failed) = tool_result_text(payload);
                        log.messages
                            .push(json!({ "role": "User", "content": { "Parts": [{
                                "ToolResult": {
                                    "tool_use_id": call,
                                    "content": { "Text": text },
                                    "is_error": failed,
                                }
                            }]}}));
                    }
                    _ => {}
                }
            }
        }
        log
    }

    /// Append a part to the model turn in progress, opening it when the last
    /// message is not one. Two model messages never sit next to each other in the
    /// conversation this produces: a step is thinking, text and tool calls, and
    /// the tool results that follow reopen a user message.
    fn model_part(&mut self, part: Value) {
        let tail_is_model = self
            .messages
            .last()
            .is_some_and(|m| m["role"] == "Model" && m["content"].get("Parts").is_some());
        if !tail_is_model {
            self.messages
                .push(json!({ "role": "Model", "content": { "Parts": [] } }));
        }
        if let Some(parts) = self
            .messages
            .last_mut()
            .and_then(|m| m["content"].get_mut("Parts"))
            .and_then(|p| p.as_array_mut())
        {
            parts.push(part);
        }
    }

    /// `meta.json`'s summary. Only the fields Ante's reader requires are written;
    /// `message_count` is what the parse fails on when it is absent, and the rest
    /// is what the picker and `/stats` show.
    ///
    /// `dir` has no fallback of its own: a log does not carry one, and Ante
    /// refuses to resume when the directory is missing (`session cwd does not
    /// exist`). So the caller's value is used as-is rather than an empty string
    /// that would look like "somewhere" and fail at resume time.
    fn meta(&self, id: &str, context: &Context) -> Value {
        let started = self
            .started
            .clone()
            .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());
        json!({
            "id": id,
            "provider": self.provider.clone().unwrap_or_else(|| context.provider.clone()),
            "model": self.model.clone().unwrap_or_else(|| context.model.clone()),
            "short_prompt": false,
            "dir": context.cwd,
            "first_user_message": self.user_inputs.first().map(|text| {
                text.split("\n\n<folder-structure>").next().unwrap_or(text).to_string()
            }).unwrap_or_default(),
            "message_count": self.messages.len(),
            "usage": {},
            "started_time": started,
            "last_active_time": self.last.clone().unwrap_or(started),
        })
    }

    /// `dialog.json` as Ante would have written it at `TurnEnd`.
    ///
    /// `system` and `told_environment` are deliberately absent: Ante treats a
    /// missing system prompt as "recompose it from the current settings", which is
    /// both correct (the instructions the user has now) and honest (this file
    /// cannot reproduce the ones from that session). A stub would be worse — it
    /// would be read back verbatim on the next resume.
    ///
    /// `marks` is absent too. It is what Ante uses to rewind to the state before a
    /// given input, and its `len` is a position in *Ante's* message list, not one
    /// this rebuild can name reliably (the log records turn ids and prompts, not
    /// where Ante put the boundary). A wrong mark would let `/undo` truncate a
    /// conversation at the wrong place; no mark only means that turn is not a
    /// rewind target. Resume itself does not need them — verified against Ante 0.3
    /// with a snapshot carrying only `messages`.
    fn dialog(&self, _id: &str) -> Option<Value> {
        if self.messages.is_empty() {
            return None;
        }
        Some(json!({
            "messages": self.messages,
            "total_tokens": 0,
            "counted_messages": self.messages.len(),
        }))
    }
}

/// A `ToolEnd` rendered the way Ante renders it into the conversation.
///
/// `.0` is the text the model reads, `.1` whether it counts as a failure. The
/// status and the exit code disagree on purpose in Ante's own writer: a command
/// that exited non-zero is `Failed`, and so is one that was cancelled.
fn tool_result_text(payload: &Value) -> (String, bool) {
    let status = payload
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    match payload.get("result_json") {
        Some(Value::Object(map)) => {
            if let Some(done) = map.get("Completed") {
                let exit = done.get("exit_code").and_then(|v| v.as_i64());
                let stdout = done
                    .get("stdout")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let stderr = done
                    .get("stderr")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let mut text = format!(
                    "status: exited\nexit_code: {}\n\nstdout:\n{stdout}",
                    exit.map(|c| c.to_string())
                        .unwrap_or_else(|| "unknown".into())
                );
                if !stderr.is_empty() {
                    text.push_str("\nstderr:\n");
                    text.push_str(stderr);
                }
                (text, status != "Completed")
            } else {
                // Any other shape is a result this writer does not model; keep it
                // as JSON rather than losing what the tool said.
                (
                    serde_json::to_string(map).unwrap_or_default(),
                    status != "Completed",
                )
            }
        }
        Some(Value::String(text)) => (text.clone(), status != "Completed"),
        _ => ("Tool call cancelled.".to_string(), true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sandbox(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("antex-repair-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("sandbox");
        dir
    }

    fn line(stamp: &str, event: Value) -> String {
        json!({ "timestamp": stamp, "id": "evt_x", "event": event }).to_string()
    }

    fn context() -> Context {
        Context {
            cwd: "/work".to_string(),
            provider: "p".to_string(),
            model: "m".to_string(),
        }
    }

    /// A real killed archive: no `SessionStart`, so no cwd and no model in the log.
    fn killed_log_text() -> String {
        [
            line(
                "2026-10-31T02:51:09Z",
                json!({ "UserInput": "first ask\n\n<folder-structure>\n- /work" }),
            ),
            line(
                "2026-10-31T02:51:09Z",
                json!({ "TurnStart": { "turn_id": "op_1" } }),
            ),
            line("2026-10-31T02:51:14Z", json!({ "Thinking": "hmm" })),
            line("2026-10-31T02:51:15Z", json!({ "AgentMessage": "looking" })),
            line(
                "2026-10-31T02:51:16Z",
                json!({ "ToolStart": { "id": "c1", "name": "Bash", "args": { "command": "ls" } } }),
            ),
        ]
        .join("\n")
    }

    fn log_text() -> String {
        [
            line("2026-10-31T02:00:00Z", json!({ "SessionStart": { "cwd": "/work", "provider": { "id": "p" }, "model": { "id": "m" } } })),
            line("2026-10-31T02:00:01Z", json!({ "TurnStart": { "turn_id": "op_1" } })),
            line("2026-10-31T02:00:02Z", json!({ "UserInput": "first ask\n\n<folder-structure>\n- /work" })),
            line("2026-10-31T02:00:03Z", json!({ "Thinking": "hmm" })),
            line("2026-10-31T02:00:04Z", json!({ "AgentMessage": "looking" })),
            line("2026-10-31T02:00:05Z", json!({ "ToolStart": { "id": "c1", "name": "Bash", "args": { "command": "ls" } } })),
            line("2026-10-31T02:00:06Z", json!({ "ToolEnd": { "tool_use_id": "c1", "tool_name": "Bash", "status": "Completed", "result_json": { "Completed": { "exit_code": 0, "stdout": "a\n" } } } })),
        ]
        .join("\n")
    }

    #[test]
    fn an_intact_archive_is_left_alone() {
        let dir = sandbox("intact");
        std::fs::write(dir.join("events.jsonl"), log_text()).unwrap();
        std::fs::write(dir.join("meta.json"), "{}").unwrap();
        std::fs::write(dir.join("dialog.json"), "{}").unwrap();
        assert_eq!(repair(&dir, &context()), Repair::Intact);
        assert_eq!(
            std::fs::read_to_string(dir.join("meta.json")).unwrap(),
            "{}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_summary_is_written_without_touching_antes_snapshot() {
        let dir = sandbox("meta-only");
        std::fs::write(dir.join("events.jsonl"), log_text()).unwrap();
        let dialog = json!({ "messages": [{ "role": "User", "content": { "Text": "kept" } }] });
        std::fs::write(dir.join("dialog.json"), dialog.to_string()).unwrap();

        assert_eq!(repair(&dir, &context()), Repair::MetaOnly);
        // Ante's snapshot is the exact context the model saw; it must survive.
        let kept: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("dialog.json")).unwrap())
                .unwrap();
        assert_eq!(kept, dialog);
        let meta: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();
        assert_eq!(meta["first_user_message"], "first ask");
        assert_eq!(meta["dir"], "/work");
        assert_eq!(meta["provider"], "p");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn both_missing_rebuilds_the_conversation_from_the_log() {
        let dir = sandbox("rebuild");
        std::fs::write(dir.join("events.jsonl"), log_text()).unwrap();

        assert_eq!(repair(&dir, &context()), Repair::Rebuilt);
        let dialog: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("dialog.json")).unwrap())
                .unwrap();
        let messages = dialog["messages"].as_array().expect("messages");
        assert_eq!(
            messages[0],
            json!({ "role": "User", "content": { "Text": "first ask" } })
        );
        assert_eq!(messages[1]["role"], "Model");
        let parts = messages[1]["content"]["Parts"].as_array().expect("parts");
        assert_eq!(parts[0]["Thinking"]["text"], "hmm");
        assert_eq!(parts[1]["Text"], "looking");
        assert_eq!(parts[2]["ToolUse"]["name"], "Bash");
        assert_eq!(messages[2]["role"], "User");
        assert_eq!(
            messages[2]["content"]["Parts"][0]["ToolResult"]["is_error"],
            false
        );
        // The tool output has to survive: it is most of what the model re-reads.
        let text = messages[2]["content"]["Parts"][0]["ToolResult"]["content"]["Text"]
            .as_str()
            .unwrap();
        assert!(text.contains("stdout:\na"), "{text}");
        // The system prompt is left to Ante to recompose.
        assert!(dialog.get("system").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_killed_log_without_a_session_start_still_gets_a_usable_summary() {
        // This is the shape a real killed archive has: the first line is the
        // prompt, and nothing in the log says where the session ran or on which
        // model. Writing those as empty strings produced a summary Ante refused
        // (`session cwd does not exist: `) after the repair had already claimed
        // success, so the fallback has to come from the caller.
        let dir = sandbox("killed");
        std::fs::write(dir.join("events.jsonl"), killed_log_text()).unwrap();

        assert_eq!(repair(&dir, &context()), Repair::Rebuilt);
        let meta: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();
        assert_eq!(meta["dir"], "/work");
        assert_eq!(meta["provider"], "p");
        assert_eq!(meta["model"], "m");
        assert_eq!(meta["first_user_message"], "first ask");
        let dialog: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("dialog.json")).unwrap())
                .unwrap();
        assert_eq!(dialog["messages"].as_array().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_log_without_prompts_is_not_rebuilt() {
        let dir = sandbox("nolog");
        std::fs::write(
            dir.join("events.jsonl"),
            line("2026-10-31T02:00:00Z", json!({ "Info": "x" })),
        )
        .unwrap();
        assert_eq!(repair(&dir, &context()), Repair::NoLog);
        assert!(!dir.join("meta.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_directory_is_not_rebuilt() {
        let dir = sandbox("empty");
        assert_eq!(repair(&dir, &context()), Repair::NoLog);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_temporary_file_is_left_behind() {
        let dir = sandbox("tmp");
        std::fs::write(dir.join("events.jsonl"), log_text()).unwrap();
        assert_eq!(repair(&dir, &context()), Repair::Rebuilt);
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("antex-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
