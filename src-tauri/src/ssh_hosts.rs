//! Enrolled plain-Linux SSH hosts (spike, Phase 1 UI half).
//!
//! A host is `{host, user, port}` + probed engine binaries. Probing runs
//! one ssh call (`BatchMode`, no password prompts ever); attaching a
//! workspace stamps its `workspaces.meta` row with the `{"ssh": ...}`
//! shape `wsl_transport::from_workspace_meta` accepts, then re-runs the
//! history scan so the remote sessions appear in the sidebar.
//!
//! Hosts persist in `AppSettings.ssh_hosts` through the normal settings
//! save path; this module only probes and attaches.

use std::collections::HashMap;
use std::process::Stdio;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshHost {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    #[serde(default)]
    pub engines: HashMap<String, String>,
    #[serde(default)]
    pub last_ok: bool,
    #[serde(default)]
    pub last_probe: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshProbe {
    pub reachable: bool,
    pub engines: HashMap<String, String>,
    pub error: Option<String>,
}

fn validate(host: &str, user: &str, port: u16) -> Result<(), String> {
    // Same fail-closed whitelists as the transport (destination lands on
    // an ssh command line after `--`, but user/host are validated anyway).
    if !(1..=65535).contains(&port) || port == 0 {
        return Err("port must be 1-65535".into());
    }
    if !crate::engine::wsl_transport::is_safe_host(host) {
        return Err("invalid host".into());
    }
    if !crate::engine::wsl_transport::is_safe_user(user) {
        return Err("invalid user".into());
    }
    Ok(())
}

/// One ssh call: liveness marker + `command -v` per known engine binary.
/// Stdout contract parsed below: `__CCGUI_OK__`, then `bin:path` lines
/// (empty path = absent). Stderr is discarded, never piped-and-unread
/// (same deadlock rationale as the transport's upload path).
fn probe_script() -> String {
    let mut bins: Vec<String> = crate::config::ENGINES
        .iter()
        .map(|id| crate::engine::cli_binary_name(id).to_string())
        .collect();
    bins.sort();
    bins.dedup();
    let list = bins.join(" ");
    format!(
        "echo __CCGUI_OK__\nfor b in {list}; do printf '%s:' \"$b\"; command -v \"$b\" 2>/dev/null || echo; done\n"
    )
}

fn parse_probe_output(out: &str) -> (bool, HashMap<String, String>) {
    let mut engines = HashMap::new();
    let mut reachable = false;
    for line in out.lines() {
        let line = line.trim();
        if line == "__CCGUI_OK__" {
            reachable = true;
            continue;
        }
        if let Some((bin, path)) = line.split_once(':') {
            let path = path.trim();
            if !bin.is_empty() && !path.is_empty() {
                // bin -> engine id is 1:1 except qoder variants (handled by
                // cli_binary_name at send time); store under the binary name
                // and let the attach step map back to engine ids.
                engines.insert(bin.to_string(), path.to_string());
            }
        }
    }
    (reachable, engines)
}

async fn run_probe(host: &str, user: &str, port: u16) -> SshProbe {
    if let Err(e) = validate(host, user, port) {
        return SshProbe {
            reachable: false,
            engines: HashMap::new(),
            error: Some(e),
        };
    }
    let mut cmd = tokio::process::Command::new("ssh");
    cmd.arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("StrictHostKeyChecking=accept-new")
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg("-p")
        .arg(port.to_string())
        .arg("--")
        .arg(format!("{user}@{host}"))
        .arg(probe_script())
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let out = match tokio::time::timeout(std::time::Duration::from_secs(30), cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return SshProbe {
                reachable: false,
                engines: HashMap::new(),
                error: Some(format!("ssh spawn failed: {e}")),
            }
        }
        Err(_) => {
            return SshProbe {
                reachable: false,
                engines: HashMap::new(),
                error: Some("ssh probe timed out (30s)".into()),
            }
        }
    };
    if !out.status.success() {
        return SshProbe {
            reachable: false,
            engines: HashMap::new(),
            error: Some("ssh connect/auth failed (key auth required, no passwords)".into()),
        };
    }
    let (reachable, engines) = parse_probe_output(&String::from_utf8_lossy(&out.stdout));
    SshProbe {
        reachable,
        engines,
        error: if reachable {
            None
        } else {
            Some("no probe marker in output".into())
        },
    }
}

#[tauri::command]
pub async fn ssh_host_probe(host: String, user: String, port: u16) -> Result<SshProbe, String> {
    Ok(run_probe(host.trim(), user.trim(), port).await)
}

/// One concrete `Host` stanza from an ssh config file: alias (what you
/// type), effective hostname, and optional User/Port. Wildcard-only
/// stanzas (`Host *`) are skipped — they are defaults, not hosts.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConfigHost {
    pub alias: String,
    pub hostname: String,
    pub user: Option<String>,
    pub port: Option<u16>,
}

fn default_os_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .filter(|u| !u.trim().is_empty() && !u.trim().starts_with('-'))
        .unwrap_or_else(|| "root".into())
}

#[derive(Default)]
struct Stanza {
    aliases: Vec<String>,
    hostname: Option<String>,
    user: Option<String>,
    port: Option<u16>,
}

fn keyword(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    // keyword/value split on first run of whitespace or '='.
    let split = line
        .find(|c: char| c == '=' || c.is_ascii_whitespace())
        .unwrap_or(line.len());
    let (key, rest) = line.split_at(split);
    Some((key, rest.trim_start_matches(['=', ' ', '\t'])))
}

fn parse_stanzas(text: &str, out: &mut Vec<Stanza>) {
    let mut current: Option<Stanza> = None;
    let mut in_match = false;
    let flush = |current: &mut Option<Stanza>, out: &mut Vec<Stanza>| {
        if let Some(s) = current.take() {
            if !s.aliases.is_empty() {
                out.push(s);
            }
        }
    };
    for line in text.lines() {
        let Some((key, value)) = keyword(line) else {
            continue;
        };
        if key.eq_ignore_ascii_case("host") {
            flush(&mut current, out);
            in_match = false;
            let mut s = Stanza::default();
            s.aliases = value.split_whitespace().map(str::to_string).collect();
            current = Some(s);
        } else if key.eq_ignore_ascii_case("match") {
            flush(&mut current, out);
            in_match = true;
        } else if in_match {
            continue;
        } else if let Some(s) = current.as_mut() {
            if key.eq_ignore_ascii_case("hostname") {
                s.hostname = value.split_whitespace().next().map(str::to_string);
            } else if key.eq_ignore_ascii_case("user") {
                s.user = value.split_whitespace().next().map(str::to_string);
            } else if key.eq_ignore_ascii_case("port") {
                s.port = value.split_whitespace().next().and_then(|v| v.parse().ok());
            }
        }
    }
    flush(&mut current, out);
}

fn is_concrete(alias: &str) -> bool {
    !alias.is_empty() && !alias.contains(['*', '?', '!'])
}

fn expand_include(pattern: &str, ssh_dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let raw = std::path::PathBuf::from(pattern);
    let expanded = if let Some(rest) = pattern.strip_prefix("~/") {
        crate::paths::home_dir().join(rest)
    } else if raw.is_absolute() {
        raw
    } else {
        // OpenSSH resolves relative Includes against ~/.ssh.
        ssh_dir.join(raw)
    };
    let text = expanded.to_string_lossy().into_owned();
    if let Some((dir, prefix)) = text.rsplit_once('*') {
        // Single trailing-glob support (covers `conf.d/*`); full OpenSSH
        // glob syntax is a follow-up.
        if let Ok(entries) = std::fs::read_dir(format!("{dir}")) {
            let mut names: Vec<_> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with(prefix.trim_start_matches('/')))
                        && p.is_file()
                })
                .collect();
            names.sort();
            out.extend(names);
        }
    } else if expanded.is_file() {
        out.push(expanded);
    }
}

/// Read `~/.ssh/config` plus one level of `Include`s. Missing file =
/// empty list (fresh machines have no config yet), never an error.
fn read_config_files() -> Vec<String> {
    read_config_files_in(&crate::paths::home_dir().join(".ssh"))
}

fn read_config_files_in(ssh_dir: &std::path::Path) -> Vec<String> {
    let main = ssh_dir.join("config");
    let mut texts = Vec::new();
    if let Ok(text) = std::fs::read_to_string(&main) {
        // Collect includes from the main file only (one level; nested
        // includes inside included files are out of spike scope).
        let mut included = Vec::new();
        for line in text.lines() {
            if let Some((key, value)) = keyword(line) {
                if key.eq_ignore_ascii_case("include") {
                    for pat in value.split_whitespace() {
                        expand_include(pat, &ssh_dir, &mut included);
                    }
                }
            }
        }
        texts.push(text);
        for path in included {
            if let Ok(t) = std::fs::read_to_string(path) {
                texts.push(t);
            }
        }
    }
    texts
}

#[tauri::command]
pub fn ssh_config_hosts() -> Result<Vec<SshConfigHost>, String> {
    let mut stanzas = Vec::new();
    for text in read_config_files() {
        parse_stanzas(&text, &mut stanzas);
    }
    let os_user = default_os_user();
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for stanza in stanzas {
        for alias in stanza.aliases {
            if !is_concrete(&alias) || !seen.insert(alias.clone()) {
                continue;
            }
            out.push(SshConfigHost {
                hostname: stanza.hostname.clone().unwrap_or_else(|| alias.clone()),
                user: Some(
                    stanza
                        .user
                        .clone()
                        .filter(|u| !u.is_empty())
                        .unwrap_or_else(|| os_user.clone()),
                ),
                port: stanza.port.or(Some(22)),
                alias,
            });
        }
    }
    Ok(out)
}

/// Attach a workspace to an enrolled host: stamp `workspaces.meta` with
/// the `{"ssh": ...}` shape and re-run the history scan. `engine_paths`
/// comes from a fresh probe so a stale settings copy cannot strand a
/// moved binary. Creates the workspace row when missing.
#[tauri::command]
pub async fn ssh_host_attach(
    state: tauri::State<'_, crate::AppState>,
    host: String,
    user: String,
    port: u16,
    workspace_path: String,
    remote_path: String,
) -> Result<serde_json::Value, String> {
    let host = host.trim().to_string();
    let user = user.trim().to_string();
    let workspace_path = workspace_path.trim().to_string();
    let remote_path = remote_path.trim().to_string();
    validate(&host, &user, port)?;
    if workspace_path.is_empty() || remote_path.is_empty() {
        return Err("workspace path and remote path are required".into());
    }
    if !remote_path.starts_with('/') && !remote_path.starts_with("~/") {
        return Err("remote path must be absolute (or ~/…)".into());
    }
    let probe = run_probe(&host, &user, port).await;
    if !probe.reachable {
        return Err(probe.error.unwrap_or_else(|| "host unreachable".into()));
    }
    // bin name -> engine id (inverse of cli_binary_name; qoder variants
    // share one binary and resolve the same way at send time).
    let mut engine_paths = serde_json::Map::new();
    for id in crate::config::ENGINES {
        let bin = crate::engine::cli_binary_name(id);
        if let Some(path) = probe.engines.get(bin) {
            engine_paths.insert(id.to_string(), serde_json::Value::String(path.clone()));
        }
    }
    let meta = serde_json::json!({"ssh": {
        "host": host, "user": user, "port": port,
        "enginePaths": engine_paths, "workspace": remote_path,
    }})
    .to_string();
    {
        let conn = state.db.0.lock();
        let updated = conn
            .execute(
                "UPDATE workspaces SET meta=?1 WHERE path=?2",
                rusqlite::params![meta, workspace_path],
            )
            .map_err(|e| format!("workspace meta update: {e}"))?;
        if updated == 0 {
            let id = uuid::Uuid::new_v4().to_string();
            let name = std::path::Path::new(&remote_path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&remote_path)
                .to_string();
            conn.execute(
                "INSERT INTO workspaces(id, path, name, last_opened_at, sort_order, meta)
                 VALUES(?1,?2,?3,0,(SELECT COALESCE(MAX(sort_order),-1)+1 FROM workspaces),?4)",
                rusqlite::params![id, workspace_path, name, meta],
            )
            .map_err(|e| format!("workspace insert: {e}"))?;
        }
    }
    crate::history::scanner::spawn_scan(
        std::sync::Arc::clone(&state.db),
        std::sync::Arc::clone(&state.sink),
    );
    Ok(serde_json::json!({"ok": true, "engines": probe.engines}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_output_parses_marker_and_bins() {
        let (reachable, engines) = parse_probe_output(
            "__CCGUI_OK__\nmuse:/root/.local/bin/muse\ncodex:\nopencode:/usr/bin/opencode\n",
        );
        assert!(reachable);
        assert_eq!(engines.get("muse").unwrap(), "/root/.local/bin/muse");
        assert_eq!(engines.get("opencode").unwrap(), "/usr/bin/opencode");
        assert!(!engines.contains_key("codex"));
    }

    #[test]
    fn probe_output_without_marker_is_unreachable() {
        let (reachable, _) = parse_probe_output("Permission denied\n");
        assert!(!reachable);
    }

    #[test]
    fn validation_rejects_injection_shaped_input() {
        assert!(validate("-oProxyCommand=x", "root", 22).is_err());
        assert!(validate("h", "-evil", 22).is_err());
        assert!(validate("h", "root", 0).is_err());
        assert!(validate("172.16.15.168", "root", 22).is_ok());
    }

    #[test]
    fn ssh_config_parses_stanzas_skips_wildcards_and_match() {
        let text = "# comment\nHost web prod\n  HostName 10.0.0.5\n  User deploy\n  Port 2222\n\nHost *.internal\n  User svc\n\nMatch host *.x\n  User ignored\n\nHost db=alias\n";
        let mut stanzas = Vec::new();
        parse_stanzas(text, &mut stanzas);
        assert_eq!(stanzas.len(), 3);
        assert_eq!(stanzas[0].aliases, vec!["web".to_string(), "prod".to_string()]);
        assert_eq!(stanzas[0].hostname.as_deref(), Some("10.0.0.5"));
        assert_eq!(stanzas[0].user.as_deref(), Some("deploy"));
        assert_eq!(stanzas[0].port, Some(2222));
        assert!(!is_concrete("*.internal"));
        assert!(is_concrete("db=alias"));
    }

    #[test]
    fn config_files_follow_one_include_level() {
        let dir = std::env::temp_dir().join(format!("ccgui-sshcfg-{}", uuid::Uuid::new_v4().simple()));
        let ssh = dir.join(".ssh");
        std::fs::create_dir_all(ssh.join("conf.d")).unwrap();
        std::fs::write(
            ssh.join("config"),
            "Include conf.d/*\nHost main\n  HostName 10.1.0.1\n",
        )
        .unwrap();
        std::fs::write(ssh.join("conf.d").join("extra"), "Host side\n  User op\n").unwrap();
        let texts = read_config_files_in(&ssh);
        assert_eq!(texts.len(), 2);
        let mut stanzas = Vec::new();
        for t in &texts {
            parse_stanzas(t, &mut stanzas);
        }
        let aliases: Vec<_> = stanzas.iter().flat_map(|s| s.aliases.clone()).collect();
        assert_eq!(aliases, vec!["main".to_string(), "side".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_config_is_empty_not_error() {
        let dir = std::env::temp_dir().join(format!("ccgui-sshcfg-{}", uuid::Uuid::new_v4().simple()));
        assert!(read_config_files_in(&dir.join(".ssh")).is_empty());
    }

    #[test]
    fn keyword_splits_on_space_or_equals() {
        assert_eq!(keyword("HostName=example.com"), Some(("HostName", "example.com")));
        assert_eq!(keyword("  User   deploy "), Some(("User", "deploy")));
        assert!(keyword("# comment").is_none());
        assert!(keyword("").is_none());
    }

    /// Live proof against the disposable LXC target (ignored by default).
    #[tokio::test]
    #[ignore]
    async fn ssh_probe_live() {
        let target = std::env::var("CCSPIKE_SSH").expect("set CCSPIKE_SSH=user@host");
        let (user, host) = target.split_once('@').expect("user@host");
        let probe = run_probe(host, user, 22).await;
        assert!(probe.reachable, "error: {:?}", probe.error);
        assert!(probe.engines.contains_key("codex"), "engines: {:?}", probe.engines);
    }
}
