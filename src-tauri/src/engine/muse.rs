use super::{command_for_binary, images, BuiltCommand, Engine, EngineEvent, SendRequest};
use serde_json::Value;

/// Muse child transport: `muse exec --json` (verified live against
/// muse-bin 1.3.x, echo + meta providers).
///
/// Event envelope per line:
/// `{stream:{kind,id}, record_type, payload_type, payload:{kind,...}}`.
/// Text: `run.output.delta` / `payload.text`. Turn settle:
/// `run.terminal.completed` / `payload.{terminal: completed|..., text?}`.
/// Session: `stream.id` (announced on `run.lifecycle.started`).
///
/// Always [`Transport::Child`]: one-shot CLI child, so the SSH-spike
/// transport carries it with no extra work. No native Windows `muse`
/// binary exists, so remote means Linux hosts (or WSL).
pub struct MuseEngine;

/// payload_type values this parser acts on. Everything else (task.*
/// lifecycle chatter, approvals, retained facts) is intentionally ignored.
mod pt {
    pub const OUTPUT_DELTA: &str = "run.output.delta";
    pub const RUN_STARTED: &str = "run.lifecycle.started";
    pub const MODEL_CONFIGURED: &str = "run.model.configured";
    pub const TERMINAL_PREFIX: &str = "run.terminal.";
}

fn stream_id(value: &Value) -> Option<String> {
    value
        .get("stream")
        .and_then(|s| s.get("id"))
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

impl Engine for MuseEngine {
    fn id(&self) -> &'static str {
        "muse"
    }

    fn supports_images(&self) -> bool {
        true // repeatable --image PATH (remote: path must exist on the host)
    }

    fn supports_effort(&self) -> bool {
        true // --reasoning-effort, passed through verbatim like codex
    }

    fn supported_permissions(&self) -> &'static [&'static str] {
        // Headless exec cannot prompt; named --permission-profile mapping
        // is a follow-up. "auto" is today's only honest mode.
        &["auto"]
    }

    fn build_command(&self, req: &SendRequest, bin: &str) -> Result<BuiltCommand, String> {
        if req.computer_use == Some(true) {
            return Err("操作电脑不支持 muse:没有可挂载的 MCP 驱动旗标".into());
        }
        let mut cmd = command_for_binary(bin);
        cmd.arg("exec");
        cmd.arg("--json");
        // --session-id both preassigns a fresh UUID and continues an
        // existing session (verified live): pass through either way.
        let mut preassigned = None;
        if let Some(session_id) = req.session_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            cmd.arg("--session-id");
            cmd.arg(session_id);
            preassigned = Some(session_id.to_string());
        }
        if let Some(model) = req.model.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            cmd.arg("--model");
            cmd.arg(model);
        }
        if let Some(effort) = req.effort.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            cmd.arg("--reasoning-effort");
            cmd.arg(effort);
        }
        for raw in &req.images {
            if let Some(path) = images::absolutize_image_path(raw, &req.workspace) {
                cmd.arg("--image");
                cmd.arg(path);
            }
        }
        // Policy-gated workspace tools rooted at the session workspace.
        if req.workspace.as_os_str().is_empty() {
            return Err("muse 需要非空工作区(--workspace)".into());
        }
        cmd.arg("--workspace");
        cmd.arg(&req.workspace);
        // Prompt is positional after `--`: leading `-` can never parse as a
        // flag, and multiline survives (muse is a native binary, not a
        // .cmd shim, so cmd.exe argv mangling does not apply — and there is
        // no Windows muse build anyway).
        cmd.arg("--");
        cmd.arg(&req.prompt);
        Ok(BuiltCommand {
            command: cmd,
            stdin_payload: None,
            keep_stdin_open: false,
            cleanup_files: Vec::new(),
            mcp_restore: None,
            preassigned_session_id: preassigned,
        })
    }

    fn parse_line(&self, line: &str, out: &mut Vec<EngineEvent>) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let payload_type = value
            .get("payload_type")
            .and_then(Value::as_str)
            .unwrap_or("");
        let payload = value.get("payload");
        match payload_type {
            pt::OUTPUT_DELTA => {
                if let Some(text) = payload
                    .and_then(|p| p.get("text"))
                    .and_then(Value::as_str)
                {
                    if !text.is_empty() {
                        out.push(EngineEvent::Delta(text.to_string()));
                    }
                }
            }
            pt::RUN_STARTED => {
                if let Some(id) = stream_id(&value) {
                    out.push(EngineEvent::SessionId(id));
                }
            }
            pt::MODEL_CONFIGURED => {
                if let Some(model) = payload
                    .and_then(|p| p.get("model_id"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    out.push(EngineEvent::Model(model.to_string()));
                }
            }
            t if t.starts_with(pt::TERMINAL_PREFIX) => {
                let terminal = payload
                    .and_then(|p| p.get("terminal"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if terminal == "completed" {
                    out.push(EngineEvent::Done {
                        session_id: stream_id(&value),
                        usage: None, // exec stream carries no token usage
                    });
                } else {
                    // reason > text > terminal label, first non-blank wins.
                    let detail = payload
                        .and_then(|p| p.get("reason"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .or_else(|| {
                            payload
                                .and_then(|p| p.get("text"))
                                .and_then(Value::as_str)
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                        })
                        .unwrap_or(terminal);
                    out.push(EngineEvent::Error(format!("muse run {terminal}: {detail}")));
                }
            }
            _ => {}
        }
    }
}

/// Test helper: collect parse_line events for whole captured lines.
#[cfg(test)]
fn parse_all(engine: &MuseEngine, lines: &[&str]) -> Vec<EngineEvent> {
    use super::Engine;
    let mut out = Vec::new();
    for line in lines {
        engine.parse_line(line, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn envelope(payload_type: &str, payload: Value, stream_id: &str) -> String {
        json!({
            "schema_version": 1,
            "stream": {"kind": "session", "id": stream_id},
            "sequence": 1,
            "record_type": "event",
            "payload_type": payload_type,
            "payload": payload,
        })
        .to_string()
    }

    #[test]
    fn delta_text_streams_and_noise_is_ignored() {
        let engine = MuseEngine;
        let sid = "01a0d500-0000-4000-8000-000000000001";
        let out = parse_all(
            &engine,
            &[
                "muse: workspace root: /tmp (cwd default)",
                "",
                "not json at all",
                &envelope(
                    "task.lifecycle.started",
                    json!({"kind": "started"}),
                    sid,
                ),
                &envelope(
                    pt::OUTPUT_DELTA,
                    json!({"kind": "run_output_delta", "text": "PO"}),
                    sid,
                ),
                &envelope(pt::OUTPUT_DELTA, json!({"kind": "run_output_delta", "text": ""}), sid),
                &envelope(
                    pt::OUTPUT_DELTA,
                    json!({"kind": "run_output_delta", "text": "NG"}),
                    sid,
                ),
            ],
        );
        let deltas: Vec<_> = out
            .iter()
            .filter_map(|e| match e {
                EngineEvent::Delta(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, vec!["PO".to_string(), "NG".to_string()]);
    }

    #[test]
    fn run_start_announces_session_and_model_is_reported() {
        let engine = MuseEngine;
        let sid = "01a0d500-0000-4000-8000-000000000001";
        let out = parse_all(
            &engine,
            &[
                &envelope(pt::RUN_STARTED, json!({"kind": "run_started"}), sid),
                &envelope(
                    pt::MODEL_CONFIGURED,
                    json!({"kind": "run_model_configured", "model_id": "muse-spark-1.3-contributor"}),
                    sid,
                ),
            ],
        );
        assert!(out.iter().any(|e| matches!(
            e,
            EngineEvent::SessionId(id) if id == sid
        )));
        assert!(out.iter().any(|e| matches!(
            e,
            EngineEvent::Model(m) if m == "muse-spark-1.3-contributor"
        )));
    }

    #[test]
    fn terminal_completed_settles_done_failed_errors() {
        let engine = MuseEngine;
        let sid = "01a0d500-0000-4000-8000-000000000001";
        let out = parse_all(
            &engine,
            &[&envelope(
                "run.terminal.completed",
                json!({"kind": "run_terminal", "terminal": "completed", "text": "PONG"}),
                sid,
            )],
        );
        assert!(out.iter().any(|e| matches!(
            e,
            EngineEvent::Done { session_id: Some(id), .. } if id == sid
        )));
        let out = parse_all(
            &engine,
            &[&envelope(
                "run.terminal.failed",
                json!({"kind": "run_terminal", "terminal": "failed", "reason": "boom happened"}),
                sid,
            )],
        );
        let errs: Vec<_> = out
            .iter()
            .filter_map(|e| match e {
                EngineEvent::Error(m) => Some(m.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("boom happened"), "got: {}", errs[0]);
    }

    /// Live proof: full build_command → ssh wrap → remote `muse exec`
    /// → parse_line path against the disposable LXC target. Not run by
    /// default (`cargo test muse_remote -- --ignored --nocapture`) with:
    /// `CCSPIKE_SSH=user@host CCSPIKE_MUSE_BIN=/path/to/muse`,
    /// key auth in place, `/tmp/spike-ws` present remotely.
    #[tokio::test]
    #[ignore]
    async fn muse_remote_exec_roundtrip() {
        use std::collections::HashMap;
        use std::path::PathBuf;
        use tokio::io::{AsyncBufReadExt, BufReader};

        let target = std::env::var("CCSPIKE_SSH").expect("set CCSPIKE_SSH=user@host");
        let bin = std::env::var("CCSPIKE_MUSE_BIN").expect("set CCSPIKE_MUSE_BIN=/path/to/muse");
        let (user, host) = target.split_once('@').expect("user@host");
        let engine = MuseEngine;
        let req = SendRequest {
            session_id: None,
            workspace: PathBuf::from("/tmp/spike-ws"),
            prompt: "reply with exactly the word MUSPIKE and nothing else".into(),
            images: vec![],
            model: None,
            effort: Some("minimal".into()),
            service_tier: None,
            permission: None,
            additional_dirs: vec![],
            provider_id: None,
            computer_use: None,
            allowed_tools: None,
        };
        // bin is the engine name (production passes engine_bin(), whose
        // basename hits the remote engine_paths table the same way).
        let built = engine.build_command(&req, "muse").unwrap();
        let tp = super::super::wsl_transport::WslTransport {
            host: host.into(),
            port: 22,
            user: user.into(),
            distro: None,
            control_path: None,
            engine_paths: HashMap::from([("muse".to_string(), bin)]),
            workspace: Some("/tmp/spike-ws".into()),
        };
        let wrapped = super::super::wsl_transport::wrap(built.command, &tp)
            .await
            .expect("ssh wrap");
        let mut cmd = wrapped.command;
        cmd.stdout(std::process::Stdio::piped());
        let mut child = cmd.spawn().expect("spawn ssh");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut reader = BufReader::new(stdout);
        let mut events = Vec::new();
        let mut done = false;
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await.expect("read stdout");
            if n == 0 {
                break;
            }
            engine.parse_line(&line, &mut events);
            if events.iter().any(|e| matches!(e, EngineEvent::Done { .. })) {
                done = true;
                break;
            }
        }
        let _ = child.start_kill();
        assert!(done, "no Done event; events: {events:?}");
        assert!(
            events.iter().any(|e| matches!(e, EngineEvent::Delta(_))),
            "no text deltas; events: {events:?}"
        );
    }

    #[test]
    fn build_command_carries_session_model_effort_images_workspace() {
        use std::path::PathBuf;
        let engine = MuseEngine;
        let req = SendRequest {
            session_id: Some("sess-1".into()),
            workspace: PathBuf::from("/tmp/ws"),
            prompt: "do the thing".into(),
            images: vec!["/tmp/ws/shot.png".into()],
            model: Some("muse-spark".into()),
            effort: Some("low".into()),
            service_tier: None,
            permission: None,
            additional_dirs: vec![],
            provider_id: None,
            computer_use: None,
            allowed_tools: None,
        };
        let built = engine.build_command(&req, "muse").unwrap();
        let argv: Vec<String> = built
            .command
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        for want in [
            "exec",
            "--json",
            "--session-id",
            "sess-1",
            "--model",
            "muse-spark",
            "--reasoning-effort",
            "low",
            "--image",
            "/tmp/ws/shot.png",
            "--workspace",
            "/tmp/ws",
            "--",
            "do the thing",
        ] {
            assert!(argv.contains(&want.to_string()), "missing {want}: {argv:?}");
        }
        assert_eq!(built.preassigned_session_id.as_deref(), Some("sess-1"));
        assert!(built.stdin_payload.is_none());
    }
}
