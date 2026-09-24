//! WSL 远程 spawn 通道 —— 把本机构造的引擎命令包装成 `ssh → wsl.exe` 执行。
//!
//! 工作区行带 `meta.wsl`(插件 `workspaces.add` 写入,见 plugin-sdk 0.3.3)时,
//! `send_message` 经此模块把引擎进程放到远程 Windows 宿主的发行版内:
//!
//! 1. 生成发行版内脚本:先 `cd` 到发行版内工作区,再以**发行版内**的引擎
//!    路径 `exec <cli> <args…>`(argv[0] 经 `enginePaths` 表或远端
//!    `command -v` 解析 —— 本机 macOS/Windows 路径在 Linux 里不存在);
//! 2. 一次 `ssh <host> "wsl.exe -d <distro> -- tee /tmp/ccgui-wsl-<id>.sh"`
//!    经 stdin 明文写入脚本(脚本内容不进命令串,零转义需求);
//! 3. 真正的 run 进程 = `ssh <host> "wsl.exe -d <distro> -- bash /tmp/….sh"`
//!    (命令串只含固定词,无 `$`/`|`/`>`/反引号,cmd/PowerShell 均惰性),
//!    脚本以 `exec` 开头(bash 被替换为 CLI):stdin(prompt payload)与
//!    stdout(NDJSON 流)原样直通本管道,进程组 kill → ssh 断 → 远端
//!    SIGHUP 直达 CLI,中断语义与本地一致。
//!
//! 认证:key 直连(BatchMode),或插件预先建立的 SSH ControlMaster
//! (`controlPath` 随 meta 传入,本地 ssh 复用既有主连接,免密码交互)。
//! 远端脚本落盘 harmless(只含命令行,prompt 走 stdin 不落盘)。

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

/// 远程短调用(脚本上传/模型目录/历史回放拉取)的全局 deadline。
/// ssh_options 的 ConnectTimeout 只覆盖 TCP 建连;远端建连后挂起(wsl.exe
/// 无响应/管道写满)没有它会让 tauri 命令 future 永不 resolve,前端无
/// 取消路径,发送/模型选择/历史翻页全部卡死到 app 重启。引擎会话本体
/// (wrap 出的长驻 run 命令)不在此列——它的存活期就是 turn 的存活期。
const REMOTE_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// `meta.wsl` 的形状(插件 ccgui-plugin-wsl 写入;其他插件可同构复用)。
#[derive(Debug, Clone)]
pub struct SshTransport {
    /// 远端宿主地址(IP/主机名)。WSL 场景下是承载发行版的 Windows 主机。
    pub host: String,
    pub port: u16,
    pub user: String,
    /// 发行版名(wsl.exe -d)。None = 普通 Linux 主机,命令直跑 bash。
    pub distro: Option<String>,
    /// SSH ControlMaster 套接字路径;None = 纯 BatchMode(key 认证)。
    pub control_path: Option<String>,
    /// 引擎 bin 在发行版内的绝对路径(bin 名 → 路径,插件探针写入)。
    pub engine_paths: HashMap<String, String>,
    /// 工作区在远端主机内的路径(`cd` 目标;缺省 = 远端默认 cwd)。
    pub workspace: Option<String>,
}

/// Spike 兼容别名:调用方逐步迁移到 SshTransport。
pub type WslTransport = SshTransport;

/// 从工作区 `meta` JSON 提取传输描述;形状不符 = None(按本地工作区跑)。
/// 接受两种形状:
/// - `{"wsl": {host, user, distro, ...}}` —— 经 Windows 主机的发行版(既有行为);
/// - `{"ssh": {host, user, port?, enginePaths?, workspace?}}` —— 普通 Linux
///   主机(spike),命令直跑远端 bash。
pub fn from_workspace_meta(meta: &serde_json::Value) -> Option<WslTransport> {
    if let Some(ssh) = meta.get("ssh") {
        return parse_tp(ssh, None);
    }
    parse_tp(meta.get("wsl")?, Some(()))
}

fn parse_tp(obj: &serde_json::Value, require_distro: Option<()>) -> Option<SshTransport> {
    let host = obj.get("host")?.as_str()?.trim().to_string();
    if !is_safe_host(&host) {
        return None;
    }
    let user = obj.get("user")?.as_str()?.trim().to_string();
    if !is_safe_user(&user) {
        return None;
    }
    let distro = match require_distro {
        Some(()) => {
            let d = obj.get("distro")?.as_str()?.trim().to_string();
            if !is_safe_distro(&d) {
                return None;
            }
            Some(d)
        }
        None => None,
    };
    let engine_paths = obj
        .get("enginePaths")
        .and_then(|v| v.as_object())
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| {
                    let path = v.as_str()?.trim();
                    (!path.is_empty()).then(|| (k.clone(), path.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    Some(WslTransport {
        host,
        port: match obj.get("port") {
            Some(v) => v.as_u64()?.try_into().ok()?,
            None => 22,
        },
        user,
        distro,
        control_path: obj
            .get("controlPath")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty()),
        engine_paths,
        workspace: obj
            .get("workspace")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty()),
    })
}

/// meta.wsl 字段字符白名单 —— 这些值来自插件/IPC 写入的 db 行,之后会拼进
/// 本机 ssh argv 与远端 Windows 命令串,解析入口统一收口,违例整体按 None
/// (本地工作区)处理,fail-closed。

/// ssh 登录名:Linux 用户名字符集,且不得以 `-` 开头 —— 否则 `user@host`
/// 整体会被 ssh 的 getopt 当成选项吞掉(`-oProxyCommand=…` → 本机命令执行)。
pub(crate) fn is_safe_user(user: &str) -> bool {
    !user.is_empty()
        && !user.starts_with('-')
        && user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// 主机地址:IP/主机名/IPv6,同样不得以 `-` 开头(getopt 选项注入)。
pub(crate) fn is_safe_host(host: &str) -> bool {
    !host.is_empty()
        && !host.starts_with('-')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':'))
}

/// 发行版名:拼进远端 DefaultShell 解释的双引号串,PowerShell 下 `$(…)`/
/// 反引号、cmd 下 `%VAR%` 在引号内仍会求值 —— 只放行字母数字与 `._- `。
fn is_safe_distro(distro: &str) -> bool {
    !distro.is_empty()
        && distro
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ' '))
}

/// 发行版内工作区路径白名单(脚本内不加引号直排 `cd <ws>`:bash 对行首
/// `~` 原生 tilde 展开;空格/`$`/`;` 等一律拒绝)。send 路径的
/// build_script 与 catalog 路径的 models/wsl.rs 共用同一规则。
pub(crate) fn is_safe_workspace(ws: &str) -> bool {
    !ws.is_empty()
        && ws
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '-' | '~'))
}

/// 单个 argv 元素的 POSIX 单引号包裹(bash 脚本内)。
pub(crate) fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// 从 tokio Command 抽取 program + args + 显式 env(构造点都在本 crate 内,
/// get_envs 只含 build_command 显式设置的项,不含继承环境)。env 经脚本头
/// `export` 跨机传递(MAX_THINKING_TOKENS 等;值一律 sh_quote)。
fn program_args_env(command: &Command) -> (String, Vec<String>, Vec<(String, String)>) {
    let std_cmd = command.as_std();
    let program = std_cmd.get_program().to_string_lossy().into_owned();
    let args = std_cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let envs = std_cmd
        .get_envs()
        .filter_map(|(k, v)| {
            let key = k.to_string_lossy().into_owned();
            // env 键必须是合法 shell 标识符,否则宁可丢掉也不进脚本
            let valid = key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            let value = v?.to_string_lossy().into_owned();
            valid.then(|| (key, value))
        })
        .collect();
    (program, args, envs)
}

/// ssh 公共选项(key 认证 BatchMode;ControlMaster 存在则复用免密)。
fn ssh_options(transport: &WslTransport) -> Vec<String> {
    let mut opts = vec![
        "-o".to_string(),
        // 首连信任(TOFU):已知主机之后的改动仍会拦,但首次连接不验指纹 ——
        // LAN 上首连可被 MITM 而无感知。可用性权衡:首次接入提示交给插件
        // 连接流程;若后续要硬约束,可让 meta 携带已知主机指纹。
        "StrictHostKeyChecking=accept-new".to_string(),
        "-o".to_string(),
        "ConnectTimeout=10".to_string(),
        "-p".to_string(),
        transport.port.to_string(),
    ];
    match &transport.control_path {
        // Master connection exists → no password prompt will occur; BatchMode
        // keeps a dead master from hanging the spawn waiting on input.
        Some(cp) => {
            opts.push("-o".to_string());
            opts.push(format!("ControlPath={cp}"));
            opts.push("-o".to_string());
            opts.push("BatchMode=yes".to_string());
        }
        None => {
            opts.push("-o".to_string());
            opts.push("BatchMode=yes".to_string());
        }
    }
    opts
}

fn ssh_target(transport: &WslTransport) -> String {
    format!("{}@{}", transport.user, transport.host)
}

/// 公共 ssh 命令骨架:选项 + `--` + destination。`--` 终结 getopt 解析,
/// destination 永远不被当成选项(user/host 另有白名单,这里是纵深防御)。
fn base_ssh_command(transport: &WslTransport) -> Command {
    let mut command = Command::new("ssh");
    for opt in ssh_options(transport) {
        command.arg(opt);
    }
    command.arg("--");
    command.arg(ssh_target(transport));
    command
}

/// 远端命令串:distro 有值走 `wsl.exe -d <distro> -- …`(Windows 宿主),
/// None 则直跑远端 shell(Linux 宿主,spike)。argv 全是固定词/白名单路径,
/// 双引号包裹后整体作为 ssh 的单个远端参数。
fn remote_command_string(transport: &WslTransport, remote_argv: &[&str]) -> String {
    let joined = remote_argv
        .iter()
        .map(|a| {
            // WSL 路径下这是远端 DefaultShell 交给 wsl.exe 的串:双引号,
            // 内部不出现 $ / 反引号 / |(调用方保证安全字符集:tee/bash 与
            // UUID 路径是固定词,distro 在 from_workspace_meta 已过白名单,
            // replace 只是纵深防御)。
            format!("\"{a}\"")
        })
        .collect::<Vec<_>>()
        .join(" ");
    match &transport.distro {
        Some(distro) => {
            let distro = distro.replace('"', "");
            format!("wsl.exe -d \"{distro}\" -- {joined}")
        }
        None => joined,
    }
}

/// 把本机 argv[0] 解析成发行版内的可执行路径:
/// 1. `enginePaths` 表命中(program 的 basename 或全等 program);
/// 2. 否则交给脚本内 `command -v` 兜底解析(远端 PATH 语义)。
fn resolve_remote_program(program: &str, transport: &WslTransport) -> String {
    let base = PathBuf::from(program)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.to_string());
    if let Some(p) = transport
        .engine_paths
        .get(program)
        .or_else(|| transport.engine_paths.get(&base))
    {
        return p.clone();
    }
    format!("__RESOLVE__{base}")
}

/// 生成发行版内执行的脚本文本。workspace 路径来自插件登记(白名单见
/// is_safe_workspace),**不加引号**直排 —— bash 对行首 `~` 原生 tilde
/// 展开(tmd 2026-09-13 真机验证形态;引号内 `~` 不展开,手动 `$HOME`
/// 拼接是 Fragile 的)。
fn build_script(
    program: &str,
    args: &[String],
    envs: &[(String, String)],
    transport: &WslTransport,
) -> String {
    let mut script = String::new();
    script.push_str("set -e\n");
    // 脚本落盘即删:bash 对已打开的 fd 继续读取不受 unlink 影响(exec 替换
    // 进程镜像也能删,EXIT trap 在 exec 下不会触发,故不用 trap)。
    script.push_str("rm -f \"$0\"\n");
    // build_command 显式设置的 env 跨机传递(键已限 shell 标识符,值一律
    // 单引号包裹):MAX_THINKING_TOKENS / GROK_DISABLE_AUTOUPDATER 等。
    for (key, value) in envs {
        script.push_str(&format!("export {key}={}\n", sh_quote(value)));
    }
    if let Some(ws) = &transport.workspace {
        if !is_safe_workspace(ws) {
            return format!("echo 'workspace 路径含不支持的字符' >&2; exit 60\n");
        }
        script.push_str(&format!("cd {ws} || exit 61\n"));
    }
    let resolved = resolve_remote_program(program, transport);
    if let Some(base) = resolved.strip_prefix("__RESOLVE__") {
        // 登录 shell 解析(~/.profile 后的 PATH):非登录 `command -v`
        // 探不到 ~/.local/bin(tmd 2026-09-12 实测),引擎常装在那。
        script.push_str(&format!(
            "bin=$(bash -lc {}) || exit 62\n",
            sh_quote(&format!("command -v {base}"))
        ));
        script.push_str("exec \"$bin\"");
    } else {
        script.push_str("exec ");
        script.push_str(&sh_quote(&resolved));
    }
    for a in args {
        script.push(' ');
        script.push_str(&sh_quote(a));
    }
    script.push('\n');
    script
}

/// 把脚本经 ssh stdin 直写发行版 `tee`(脚本内容全程不进命令串,零转义
/// 需求;命令串只有固定词)。任何失败返回 Err(spawn 前失败,turn 直接报错)。
async fn upload_script(
    transport: &WslTransport,
    script_body: &str,
    remote_path: &str,
) -> Result<(), String> {
    let mut command = base_ssh_command(transport);
    command.arg(remote_command_string(transport, &["tee", remote_path]));
    // stderr 直接丢弃:从不读取却 piped 会在远端写满管缓冲时与 wait 互等
    // 死锁;错误细节本就不进报错文案(下面是固定提示)。
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|e| format!("ssh 脚本上传失败: {e}"))?;
    let wrote = if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let result = stdin.write_all(script_body.as_bytes()).await;
        stdin.shutdown().await.ok();
        drop(stdin);
        result
    } else {
        Ok(())
    };
    wrote.map_err(|e| format!("ssh 脚本写入失败: {e}"))?;
    // 全局 deadline:远端挂起时管道写满会让 wait 与 write 互等,没有超时
    // 这个 future 永不 resolve(调用链上UI 无取消路径)。超时即 kill,
    // 不留半写的远端脚本与孤儿 ssh。
    let status = match tokio::time::timeout(REMOTE_CALL_TIMEOUT, child.wait()).await {
        Ok(result) => result.map_err(|e| format!("ssh 脚本上传等待失败: {e}"))?,
        Err(_) => {
            let _ = child.start_kill();
            return Err(format!(
                "ssh 脚本上传超时({}s,检查远程主机是否无响应)",
                REMOTE_CALL_TIMEOUT.as_secs()
            ));
        }
    };
    if !status.success() {
        return Err("ssh 脚本上传失败(检查远程主机连接/认证;ControlMaster 可能已过期,请重连该主机)".to_string());
    }
    Ok(())
}

/// 包装结果:重造后的 ssh 命令 + 本地临时标记文件(run 结束清理)。
pub struct Wrapped {
    pub command: Command,
    pub cleanup_files: Vec<PathBuf>,
    /// 标记本次 spawn 不应设置本地 current_dir(远程路径在本机不存在)。
    pub skip_local_cwd: bool,
}

/// 把 build_command 产出的引擎命令包装成远程 ssh 执行。失败返回 Err。
pub async fn wrap(command: Command, transport: &WslTransport) -> Result<Wrapped, String> {
    let (program, args, envs) = program_args_env(&command);
    let script = build_script(&program, &args, &envs, transport);
    let script_id = uuid::Uuid::new_v4().simple().to_string();
    let remote_path = format!("/tmp/ccgui-wsl-{script_id}.sh");
    upload_script(transport, &script, &remote_path).await?;

    let local_tmp = std::env::temp_dir().join(format!("ccgui-wsl-{script_id}.marker"));
    std::fs::write(&local_tmp, b"").map_err(|e| format!("临时文件写入失败: {e}"))?;

    let mut wrapped = base_ssh_command(transport);
    // 脚本已明文落盘,run 串只含固定词。
    wrapped.arg(remote_command_string(transport, &["bash", &remote_path]));
    Ok(Wrapped {
        command: wrapped,
        cleanup_files: vec![local_tmp],
        skip_local_cwd: true,
    })
}

/// 供 send_message 判断工作区是否远程(直接读 db 行的 meta JSON)。
pub fn transport_from_meta_json(meta_json: Option<&str>) -> Option<WslTransport> {
    let raw = meta_json?;
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    from_workspace_meta(&value)
}

/// 供调用方在 workspace meta 表里查询(路径 → meta 文本)。
pub fn workspace_meta_json(db: &crate::db::Db, workspace_path: &str) -> Option<String> {
    let conn = db.0.lock();
    conn.query_row(
        "SELECT meta FROM workspaces WHERE path = ?1",
        [workspace_path],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

/// 供 send_message 把本地 cwd 指到一个必然存在的目录(远程工作区场景)。
pub fn fallback_cwd() -> &'static std::path::Path {
    std::path::Path::new(".")
}

/// 远程跑一段脚本并取 stdout(模型列表等短命令;上传 + 运行两步,
/// 复用 send 路径同一套 ssh/wsl 串 —— ControlMaster/key 认证同源)。
/// 返回值已剥传输层噪声(NUL 残迹与 wsl.exe 诊断行;wsl 通道特有,
/// 载荷本体不受影响),调用方无需重复处理。
pub async fn run_script_output(
    transport: &WslTransport,
    script_body: &str,
) -> Result<String, String> {
    let script_id = uuid::Uuid::new_v4().simple().to_string();
    let remote_path = format!("/tmp/ccgui-wsl-{script_id}.sh");
    // 调用方脚本同样落盘即删(bash 从已打开 fd 继续读,unlink 不影响执行)。
    let body = format!("rm -f \"$0\"\n{script_body}");
    upload_script(transport, &body, &remote_path).await?;
    let mut command = base_ssh_command(transport);
    command.arg(remote_command_string(transport, &["bash", &remote_path]));
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // 全局 deadline(理由见 REMOTE_CALL_TIMEOUT):模型目录/历史回放都走
    // 这里,远端挂起不得拖死前端。kill_on_drop:超时后 output future 被
    // 丢弃即 SIGKILL 本地 ssh(远端脚本落盘即删,无残留面)。
    command.kill_on_drop(true);
    let output = match tokio::time::timeout(REMOTE_CALL_TIMEOUT, command.output()).await {
        Ok(result) => result.map_err(|e| format!("远程命令执行失败: {e}"))?,
        Err(_) => {
            return Err(format!(
                "远程命令超时({}s,检查远程主机是否无响应)",
                REMOTE_CALL_TIMEOUT.as_secs()
            ));
        }
    };
    if !output.status.success() {
        return Err(format!(
            "远程命令退出码 {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&strip_transport_noise(output.stdout.as_slice())).to_string())
}

/// wsl.exe 传输层噪声:NUL 残迹(UTF-16LE lossy 后)与 `wsl:`/`wsl.` 开头
/// 的诊断行。行结构保留(多行输出各剥各的),载荷行原样通过。
fn strip_transport_noise(raw: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(raw).replace('\0', "");
    text.lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.starts_with("wsl:") || t.starts_with("wsl."))
        })
        .map(|l| format!("{l}\n"))
        .collect::<Vec<_>>()
        .join("")
        .into_bytes()
}

/// 工作区路径 → 传输描述一步到位(db 读 + meta 形状解析都收敛在此,
/// 调用方不再自行组合两步查找)。
pub fn transport_for_workspace(db: &crate::db::Db, workspace_path: &str) -> Option<WslTransport> {
    transport_from_meta_json(workspace_meta_json(db, workspace_path).as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tp() -> WslTransport {
        WslTransport {
            host: "10.0.0.2".into(),
            port: 22,
            user: "dev".into(),
            distro: Some("Ubuntu-22.04".into()),
            control_path: None,
            engine_paths: HashMap::from([(
                "omp".to_string(),
                "/home/dev/.local/bin/omp".to_string(),
            )]),
            workspace: Some("/home/dev/proj".into()),
        }
    }

    #[test]
    fn meta_parses_engine_paths_and_workspace() {
        let meta = json!({"wsl": {
            "host": "10.0.0.2", "port": 2222, "user": "dev", "distro": "Ubuntu",
            "controlPath": "/tmp/m", "workspace": "/home/dev/p",
            "enginePaths": {"omp": "/home/dev/.local/bin/omp"}
        }});
        let t = from_workspace_meta(&meta).unwrap();
        assert_eq!(t.port, 2222);
        assert_eq!(t.control_path.as_deref(), Some("/tmp/m"));
        assert_eq!(
            t.engine_paths.get("omp").unwrap(),
            "/home/dev/.local/bin/omp"
        );
        assert_eq!(t.workspace.as_deref(), Some("/home/dev/p"));
    }

    #[test]
    fn script_maps_bin_cd_and_quotes_args() {
        let script = build_script(
            "/Users/x/.local/bin/omp",
            &[
                "-p".into(),
                "hello world".into(),
                "--resume".into(),
                "s-1".into(),
            ],
            &[],
            &tp(),
        );
        assert!(script.starts_with("set -e\n"));
        assert!(script.contains("cd /home/dev/proj || exit 61"));
        // argv[0] basename "omp" 命中 enginePaths → 发行版内路径
        assert!(script.contains("exec '/home/dev/.local/bin/omp'"));
        assert!(script.contains("'hello world'"));
        // 本机 mac 路径绝不残留
        assert!(!script.contains("/Users/"));
    }

    #[test]
    fn script_keeps_tilde_unquoted_for_native_expansion() {
        let mut t = tp();
        t.workspace = Some("~/code/proj".into());
        let script = build_script("omp", &[], &[], &t);
        assert!(script.contains("cd ~/code/proj || exit 61"));
        // 引号包裹会杀死 bash 的 tilde 展开 —— 绝不能出现
        assert!(!script.contains("\"~/"));
    }

    #[test]
    fn script_rejects_space_paths() {
        let mut t = tp();
        t.workspace = Some("/home/dev/my proj".into());
        let script = build_script("omp", &[], &[], &t);
        assert!(script.contains("exit 60"));
    }

    #[test]
    fn script_falls_back_to_command_v() {
        let script = build_script("kimi", &[], &[], &tp());
        assert!(script.contains("bin=$(bash -lc 'command -v kimi') || exit 62"));
        assert!(script.contains("exec \"$bin\""));
    }

    #[test]
    fn remote_command_string_is_shell_inert() {
        let s = remote_command_string(&tp(), &["bash", "/tmp/x.sh"]);
        assert_eq!(s, "wsl.exe -d \"Ubuntu-22.04\" -- \"bash\" \"/tmp/x.sh\"");
        assert!(!s.contains('$') && !s.contains('|') && !s.contains('`'));
    }

    #[test]
    fn meta_rejects_ssh_option_injection_user() {
        // user 以 `-` 开头:`user@host` 会被 ssh getopt 吞成 -o 选项
        // (ProxyCommand → 本机命令执行),必须整个拒绝。
        let meta = json!({"wsl": {
            "host": "10.0.0.2", "user": "-oProxyCommand=/tmp/evil@x", "distro": "Ubuntu"
        }});
        assert!(from_workspace_meta(&meta).is_none());
    }

    #[test]
    fn meta_rejects_dash_host_and_bad_chars() {
        for host in ["-oProxyCommand=x", "ho st", "ho;st", "ho`id`st"] {
            let meta = json!({"wsl": { "host": host, "user": "dev", "distro": "Ubuntu" }});
            assert!(from_workspace_meta(&meta).is_none(), "host: {host}");
        }
        // IPv6 / 主机名 / 带点用户名为合法形态
        let meta = json!({"wsl": {
            "host": "::1", "user": "dev.ops-1_x", "distro": "Ubuntu"
        }});
        assert!(from_workspace_meta(&meta).is_some());
    }

    #[test]
    fn meta_rejects_distro_shell_chars() {
        // PowerShell 双引号内 $(…)/反引号仍求值,cmd 下 %VAR% 仍展开
        for distro in ["Ubuntu$(id)", "u`id`", "u%PATH%", "u\"&id", "u|id"] {
            let meta = json!({"wsl": { "host": "10.0.0.2", "user": "dev", "distro": distro }});
            assert!(from_workspace_meta(&meta).is_none(), "distro: {distro}");
        }
        let meta = json!({"wsl": {
            "host": "10.0.0.2", "user": "dev", "distro": "Ubuntu-22.04 LTS"
        }});
        assert!(from_workspace_meta(&meta).is_some());
    }

    #[test]
    fn workspace_allowlist() {
        assert!(is_safe_workspace("/home/dev/proj"));
        assert!(is_safe_workspace("~/code/proj"));
        assert!(!is_safe_workspace("/home/dev/my proj"));
        assert!(!is_safe_workspace("/a;id"));
        assert!(!is_safe_workspace("/a$(id)"));
        assert!(!is_safe_workspace(""));
    }

    #[test]
    fn script_exports_env_and_self_deletes() {
        let envs = vec![("MAX_THINKING_TOKENS".to_string(), "65536".to_string())];
        let script = build_script("omp", &[], &envs, &tp());
        // 落盘即删(exec 下 trap 不触发,unlink 已打开 fd 的脚本仍然安全)
        assert!(script.contains("rm -f \"$0\""));
        assert!(script.contains("export MAX_THINKING_TOKENS='65536'"));
        // 值含单引号/空白也走 sh_quote,不直接拼接
        let envs = vec![("X_Y".to_string(), "a'b c".to_string())];
        let script = build_script("omp", &[], &envs, &tp());
        assert!(script.contains("export X_Y='a'\\''b c'"));
    }

    #[test]
    fn ssh_command_separates_destination_with_double_dash() {
        let cmd = base_ssh_command(&tp());
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let dashdash = args.iter().position(|a| a == "--").expect("missing --");
        // `--` 之后紧跟 destination,且 destination 之前没有任何非选项值
        assert_eq!(args[dashdash + 1], "dev@10.0.0.2");
        assert!(args[..dashdash]
            .iter()
            .all(|a| a.starts_with('-') || !a.contains('@')));
    }

    // ---- spike: plain-Linux SSH hosts (no distro) ----

    fn linux_tp() -> WslTransport {
        WslTransport {
            host: "172.16.15.168".into(),
            port: 22,
            user: "root".into(),
            distro: None,
            control_path: None,
            engine_paths: HashMap::from([(
                "codex".to_string(),
                "/usr/local/bin/codex".to_string(),
            )]),
            workspace: Some("/tmp/spike-ws".into()),
        }
    }

    #[test]
    fn ssh_meta_parses_without_distro() {
        let meta = json!({"ssh": {
            "host": "172.16.15.168", "user": "root",
            "workspace": "/tmp/spike-ws",
            "enginePaths": {"codex": "/usr/local/bin/codex"}
        }});
        let t = from_workspace_meta(&meta).unwrap();
        assert_eq!(t.host, "172.16.15.168");
        assert_eq!(t.port, 22);
        assert!(t.distro.is_none());
        assert_eq!(t.workspace.as_deref(), Some("/tmp/spike-ws"));
        assert_eq!(
            t.engine_paths.get("codex").unwrap(),
            "/usr/local/bin/codex"
        );
    }

    #[test]
    fn ssh_meta_rejects_bad_host_but_wsl_still_needs_distro() {
        assert!(from_workspace_meta(&json!({"ssh": {"host": "-evil", "user": "root"}})).is_none());
        assert!(from_workspace_meta(&json!({"wsl": {"host": "h", "user": "u"}})).is_none());
    }

    #[test]
    fn remote_command_wraps_wsl_but_passes_linux_through() {
        let wsl = remote_command_string(&tp(), &["bash", "/tmp/x.sh"]);
        assert_eq!(wsl, "wsl.exe -d \"Ubuntu-22.04\" -- \"bash\" \"/tmp/x.sh\"");
        let linux = remote_command_string(&linux_tp(), &["bash", "/tmp/x.sh"]);
        assert_eq!(linux, "\"bash\" \"/tmp/x.sh\"");
        let tee = remote_command_string(&linux_tp(), &["tee", "/tmp/x.sh"]);
        assert_eq!(tee, "\"tee\" \"/tmp/x.sh\"");
    }

    #[test]
    fn linux_script_resolves_engine_bin_and_cds() {
        let script = build_script("codex", &["exec".to_string()], &[], &linux_tp());
        assert!(script.contains("cd /tmp/spike-ws || exit 61"));
        assert!(script.contains("exec '/usr/local/bin/codex' 'exec'"));
    }

    /// Live proof against the disposable LXC target. Not run by default:
    /// cargo test -p <crate> ssh_linux_roundtrip -- --ignored --nocapture
    /// with CCSPIKE_SSH="user@host" and key-based auth in place.
    #[tokio::test]
    #[ignore]
    async fn ssh_linux_roundtrip() {
        let target = std::env::var("CCSPIKE_SSH").expect("set CCSPIKE_SSH=user@host");
        let (user, host) = target.split_once('@').expect("user@host");
        let t = WslTransport {
            host: host.into(),
            port: 22,
            user: user.into(),
            distro: None,
            control_path: None,
            engine_paths: HashMap::new(),
            workspace: None,
        };
        let out = run_script_output(&t, "echo PROBE_OK").await.expect("ssh run");
        assert!(out.trim_end().ends_with("PROBE_OK"), "got: {out:?}");
    }
}
