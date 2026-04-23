use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config;
use crate::runtime;

pub const DEFAULT_PORT: u16 = 18_789;
const CLI_MARKER: &str = "OneClaw CLI";
const RC_BLOCK_START: &str = "# >>> oneclaw-cli >>>";
const RC_BLOCK_END: &str = "# <<< oneclaw-cli <<<";
const LOGIN_AGENT_LABEL: &str = "cn.oneclaw.setup.daemon";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DetectionResult {
    pub port_in_use: bool,
    pub port_process: String,
    pub port_pid: u32,
    pub global_installed: bool,
    pub global_path: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictParams {
    pub action: String,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LaunchAtLoginState {
    pub supported: bool,
    pub enabled: bool,
}

pub fn detect_existing_installation(port: u16) -> Result<DetectionResult> {
    let port_in_use = is_port_in_use(port);
    let (port_pid, port_process) = if port_in_use {
        detect_port_process(port).unwrap_or((0, String::new()))
    } else {
        (0, String::new())
    };
    let (global_installed, global_path) = detect_global_openclaw();

    Ok(DetectionResult {
        port_in_use,
        port_process,
        port_pid,
        global_installed,
        global_path,
    })
}

pub fn resolve_conflict(params: &ConflictParams) -> Result<()> {
    if params.action != "uninstall" {
        return Err(anyhow!("不支持的冲突处理动作: {}", params.action));
    }
    uninstall_gateway_daemon_best_effort();
    if let Some(pid) = params.pid.filter(|pid| *pid > 0) {
        kill_pid(pid)?;
    }
    uninstall_global_openclaw_best_effort();
    Ok(())
}

pub fn detect_port_pid(port: u16) -> Option<u32> {
    detect_port_process(port).map(|(pid, _)| pid)
}

pub fn get_launch_at_login_state() -> LaunchAtLoginState {
    let supported = cfg!(target_os = "macos") || cfg!(target_os = "windows");
    LaunchAtLoginState {
        supported,
        enabled: launch_at_login_marker_path().exists(),
    }
}

pub fn set_launch_at_login_enabled(enabled: bool) -> Result<()> {
    write_oneclaw_preference("launchAtLogin", enabled)?;
    if cfg!(target_os = "macos") {
        set_launch_at_login_macos(enabled)?;
    } else if cfg!(target_os = "windows") {
        set_launch_at_login_windows(enabled)?;
    }
    Ok(())
}

pub fn is_pid_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    if cfg!(windows) {
        return Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}")])
            .output()
            .ok()
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|stdout| stdout.contains(&pid.to_string()))
            .unwrap_or(false);
    }

    Command::new("ps")
        .args(["-p", &pid.to_string()])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub fn kill_pid_force(pid: u32) -> Result<()> {
    kill_pid(pid)
}

pub fn install_cli_best_effort() -> Result<()> {
    let layout = runtime::resolve_runtime_layout()?;
    runtime::validate_runtime_layout(&layout)?;
    if cfg!(windows) {
        install_cli_windows(&layout)?;
    } else {
        install_cli_posix(&layout)?;
    }
    ensure_clawhub_wrapper(&layout)?;
    write_oneclaw_preference("cliPreference", "installed")?;
    Ok(())
}

pub fn uninstall_cli_best_effort() -> Result<()> {
    if cfg!(windows) {
        uninstall_cli_windows()?;
    } else {
        uninstall_cli_posix()?;
    }
    write_oneclaw_preference("cliPreference", "uninstalled")?;
    Ok(())
}

pub fn ensure_clawhub_wrapper(layout: &runtime::RuntimeLayout) -> Result<()> {
    if !layout.clawhub_entry.exists() {
        return Ok(());
    }
    let bin_dir = user_bin_dir();
    fs::create_dir_all(&bin_dir)?;
    let workdir = config::state_dir().join("workspace");
    fs::create_dir_all(&workdir)?;

    if cfg!(windows) {
        let wrapper = build_clawhub_windows_wrapper(&layout.node_bin, &layout.clawhub_entry, &workdir);
        fs::write(bin_dir.join("clawhub.cmd"), wrapper)?;
    } else {
        let wrapper = build_clawhub_posix_wrapper(&layout.node_bin, &layout.clawhub_entry, &workdir);
        let path = bin_dir.join("clawhub");
        fs::write(&path, wrapper)?;
        chmod_executable(&path)?;
    }
    Ok(())
}

pub fn user_bin_dir() -> PathBuf {
    config::state_dir().join("bin")
}

fn install_cli_posix(layout: &runtime::RuntimeLayout) -> Result<()> {
    let bin_dir = user_bin_dir();
    fs::create_dir_all(&bin_dir)?;
    let wrapper_path = bin_dir.join("openclaw");
    fs::write(&wrapper_path, build_posix_openclaw_wrapper(&layout.node_bin, &layout.gateway_entry))?;
    chmod_executable(&wrapper_path)?;
    for rc_path in resolve_posix_rc_paths() {
        upsert_rc_block(&rc_path, &bin_dir)?;
    }
    Ok(())
}

fn uninstall_cli_posix() -> Result<()> {
    let wrapper_path = user_bin_dir().join("openclaw");
    remove_managed_wrapper(&wrapper_path)?;
    let clawhub_path = user_bin_dir().join("clawhub");
    let _ = fs::remove_file(clawhub_path);
    for rc_path in resolve_posix_rc_paths() {
        remove_rc_block(&rc_path)?;
    }
    Ok(())
}

fn install_cli_windows(layout: &runtime::RuntimeLayout) -> Result<()> {
    let bin_dir = user_bin_dir();
    fs::create_dir_all(&bin_dir)?;
    fs::write(
        bin_dir.join("openclaw.cmd"),
        build_windows_openclaw_wrapper(&layout.node_bin, &layout.gateway_entry),
    )?;
    Ok(())
}

fn uninstall_cli_windows() -> Result<()> {
    remove_managed_wrapper(&user_bin_dir().join("openclaw.cmd"))?;
    let _ = fs::remove_file(user_bin_dir().join("clawhub.cmd"));
    Ok(())
}

fn build_posix_openclaw_wrapper(node_bin: &Path, entry: &Path) -> String {
    let safe_node = escape_for_posix_double_quoted(&node_bin.to_string_lossy());
    let safe_entry = escape_for_posix_double_quoted(&entry.to_string_lossy());
    let entry_check = if entry.to_string_lossy().contains(".asar") {
        String::new()
    } else {
        [
            "if [ ! -f \"$APP_ENTRY\" ]; then",
            "  echo \"Error: OneClaw entry not found at $APP_ENTRY\" >&2",
            "  exit 127",
            "fi",
        ]
        .join("\n")
            + "\n"
    };
    format!(
        "#!/usr/bin/env bash\n# {CLI_MARKER} - auto-generated, do not edit\nAPP_NODE=\"{safe_node}\"\nAPP_ENTRY=\"{safe_entry}\"\nif [ ! -f \"$APP_NODE\" ]; then\n  echo \"Error: OneClaw not found at $APP_NODE\" >&2\n  exit 127\nfi\n{entry_check}export OPENCLAW_NO_RESPAWN=1\nexec \"$APP_NODE\" \"$APP_ENTRY\" \"$@\"\n"
    )
}

fn build_windows_openclaw_wrapper(node_bin: &Path, entry: &Path) -> String {
    let safe_node = escape_for_cmd_set_value(&node_bin.to_string_lossy());
    let safe_entry = escape_for_cmd_set_value(&entry.to_string_lossy());
    format!(
        "@echo off\r\nREM {CLI_MARKER} - auto-generated, do not edit\r\nsetlocal\r\nset \"APP_NODE={safe_node}\"\r\nset \"APP_ENTRY={safe_entry}\"\r\nif not exist \"%APP_NODE%\" (\r\n  echo Error: OneClaw Node runtime not found. 1>&2\r\n  exit /b 127\r\n)\r\nset \"OPENCLAW_NO_RESPAWN=1\"\r\n\"%APP_NODE%\" \"%APP_ENTRY%\" %*\r\nexit /b %errorlevel%\r\n"
    )
}

fn build_clawhub_posix_wrapper(node_bin: &Path, entry: &Path, workdir: &Path) -> String {
    let safe_node = escape_for_posix_double_quoted(&node_bin.to_string_lossy());
    let safe_entry = escape_for_posix_double_quoted(&entry.to_string_lossy());
    let safe_workdir = escape_for_posix_double_quoted(&workdir.to_string_lossy());
    format!(
        "#!/usr/bin/env bash\n# OneClaw clawhub CLI - auto-generated, do not edit\nAPP_NODE=\"{safe_node}\"\nAPP_ENTRY=\"{safe_entry}\"\nAPP_WORKDIR=\"{safe_workdir}\"\nexec \"$APP_NODE\" \"$APP_ENTRY\" --workdir \"$APP_WORKDIR\" \"$@\"\n"
    )
}

fn build_clawhub_windows_wrapper(node_bin: &Path, entry: &Path, workdir: &Path) -> String {
    let safe_node = escape_for_cmd_set_value(&node_bin.to_string_lossy());
    let safe_entry = escape_for_cmd_set_value(&entry.to_string_lossy());
    let safe_workdir = escape_for_cmd_set_value(&workdir.to_string_lossy());
    format!(
        "@echo off\r\nREM OneClaw clawhub CLI - auto-generated, do not edit\r\nsetlocal\r\nset \"APP_NODE={safe_node}\"\r\nset \"APP_ENTRY={safe_entry}\"\r\nset \"APP_WORKDIR={safe_workdir}\"\r\n\"%APP_NODE%\" \"%APP_ENTRY%\" --workdir \"%APP_WORKDIR%\" %*\r\nexit /b %errorlevel%\r\n"
    )
}

fn resolve_posix_rc_paths() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir);
    let Some(home) = home else {
        return Vec::new();
    };
    vec![home.join(".zprofile"), home.join(".bash_profile")]
}

fn build_rc_block(bin_dir: &Path) -> String {
    let safe_bin = escape_for_posix_double_quoted(&bin_dir.to_string_lossy());
    [
        RC_BLOCK_START.to_string(),
        "case \":$PATH:\" in".to_string(),
        format!("  *:\"{safe_bin}\":*) ;;"),
        format!("  *) export PATH=\"{safe_bin}:$PATH\" ;;"),
        "esac".to_string(),
        RC_BLOCK_END.to_string(),
    ]
    .join("\n")
}

fn upsert_rc_block(rc_path: &Path, bin_dir: &Path) -> Result<()> {
    let current = fs::read_to_string(rc_path).unwrap_or_default();
    let stripped = strip_managed_rc_block(&current).0;
    let base = stripped.trim_end();
    let next = if base.is_empty() {
        format!("{}\n", build_rc_block(bin_dir))
    } else {
        format!("{base}\n\n{}\n", build_rc_block(bin_dir))
    };
    if next != current {
        if let Some(parent) = rc_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(rc_path, next)?;
    }
    Ok(())
}

fn remove_rc_block(rc_path: &Path) -> Result<()> {
    if !rc_path.exists() {
        return Ok(());
    }
    let current = fs::read_to_string(rc_path)?;
    let (stripped, removed) = strip_managed_rc_block(&current);
    if removed {
        let next = if stripped.trim().is_empty() {
            String::new()
        } else {
            format!("{}\n", stripped.trim_end())
        };
        fs::write(rc_path, next)?;
    }
    Ok(())
}

fn strip_managed_rc_block(content: &str) -> (String, bool) {
    let mut output = Vec::new();
    let mut pending = Vec::new();
    let mut in_block = false;
    let mut removed = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if !in_block && trimmed == RC_BLOCK_START {
            in_block = true;
            pending.push(line);
            continue;
        }
        if in_block {
            pending.push(line);
            if trimmed == RC_BLOCK_END {
                in_block = false;
                removed = true;
                pending.clear();
            }
            continue;
        }
        output.push(line);
    }

    if in_block {
        output.extend(pending);
    }

    (output.join("\n"), removed)
}

fn remove_managed_wrapper(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let content = fs::read_to_string(path).unwrap_or_default();
    if content.contains(CLI_MARKER) || content.contains("OneClaw clawhub CLI") {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn set_launch_at_login_macos(enabled: bool) -> Result<()> {
    let plist = launch_at_login_marker_path();
    if !enabled {
        let service = format!("{}/{}", launchctl_gui_domain(), LOGIN_AGENT_LABEL);
        let _ = Command::new("launchctl")
            .args(["bootout", &service])
            .status();
        let _ = fs::remove_file(plist);
        return Ok(());
    }

    let exe = std::env::current_exe().context("无法定位 claw-setup 可执行文件")?;
    let state_dir = config::state_dir();
    let resources_dir = runtime::resolve_runtime_layout()
        .ok()
        .map(|layout| layout.resources_dir)
        .unwrap_or_default();
    if let Some(parent) = plist.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        &plist,
        launch_agent_plist(&exe, &state_dir, &resources_dir),
    )?;
    let domain = launchctl_gui_domain();
    let plist_text = plist.to_string_lossy().to_string();
    let _ = Command::new("launchctl")
        .args(["bootstrap", &domain, &plist_text])
        .status();
    Ok(())
}

fn set_launch_at_login_windows(enabled: bool) -> Result<()> {
    let marker = launch_at_login_marker_path();
    if let Some(parent) = marker.parent() {
        fs::create_dir_all(parent)?;
    }
    if enabled {
        fs::write(marker, "enabled\n")?;
    } else {
        let _ = fs::remove_file(marker);
    }
    Ok(())
}

fn launch_at_login_marker_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{LOGIN_AGENT_LABEL}.plist"))
    } else {
        config::state_dir().join("launch-at-login.enabled")
    }
}

fn launch_agent_plist(exe: &Path, state_dir: &Path, resources_dir: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LOGIN_AGENT_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>--daemon</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>OPENCLAW_STATE_DIR</key>
    <string>{}</string>
    <key>ONECLAW_RESOURCES_DIR</key>
    <string>{}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{}</string>
  <key>StandardErrorPath</key>
  <string>{}</string>
</dict>
</plist>
"#,
        xml_escape(&exe.to_string_lossy()),
        xml_escape(&state_dir.to_string_lossy()),
        xml_escape(&resources_dir.to_string_lossy()),
        xml_escape(&config::state_dir().join("app.log").to_string_lossy()),
        xml_escape(&config::state_dir().join("app.log").to_string_lossy()),
    )
}

fn launchctl_gui_domain() -> String {
    format!("gui/{}", unsafe { libc_getuid() })
}

fn is_port_in_use(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_err()
}

fn detect_global_openclaw() -> (bool, String) {
    for command in ["openclaw", "openclaw-cn"] {
        if let Some(path) = find_command(command) {
            if !is_oneclaw_wrapper(&path) {
                return (true, path.to_string_lossy().to_string());
            }
        }
    }
    (false, String::new())
}

fn find_command(name: &str) -> Option<PathBuf> {
    let binary = if cfg!(windows) { "where" } else { "which" };
    let output = Command::new(binary).arg(name).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().next().map(|line| PathBuf::from(line.trim())).filter(|path| !path.as_os_str().is_empty())
}

fn is_oneclaw_wrapper(path: &Path) -> bool {
    fs::read_to_string(path)
        .map(|content| content.contains(CLI_MARKER))
        .unwrap_or(false)
}

fn detect_port_process(port: u16) -> Option<(u32, String)> {
    if cfg!(windows) {
        detect_port_process_windows(port)
    } else {
        detect_port_process_unix(port)
    }
}

fn detect_port_process_unix(port: u16) -> Option<(u32, String)> {
    let output = Command::new("lsof")
        .args(["-i", &format!(":{port}"), "-sTCP:LISTEN", "-t"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let pid = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()?
        .trim()
        .parse::<u32>()
        .ok()?;
    let name = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    Some((pid, name))
}

fn detect_port_process_windows(port: u16) -> Option<(u32, String)> {
    let output = Command::new("netstat").arg("-ano").output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let port_suffix = format!(":{port}");
    for line in stdout.lines() {
        if !line.contains("LISTENING") || !line.contains(&port_suffix) {
            continue;
        }
        let pid = line.split_whitespace().last()?.parse::<u32>().ok()?;
        return Some((pid, String::new()));
    }
    None
}

fn kill_pid(pid: u32) -> Result<()> {
    let status = if cfg!(windows) {
        Command::new("taskkill")
            .args(["/pid", &pid.to_string(), "/f", "/t"])
            .status()?
    } else {
        Command::new("kill").args(["-9", &pid.to_string()]).status()?
    };
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("终止进程失败: pid={pid}"))
    }
}

pub fn uninstall_gateway_daemon_best_effort() {
    if cfg!(target_os = "macos") {
        let domain = launchctl_gui_domain();
        for label in ["ai.openclaw.gateway", "ai.openclaw.node"] {
            let _ = Command::new("launchctl").args(["bootout", &format!("{domain}/{label}")]).status();
            let path = dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{label}.plist"));
            let _ = fs::remove_file(path);
        }
    }
    remove_openclaw_lockfiles();
}

fn uninstall_global_openclaw_best_effort() {
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    for package in ["openclaw", "openclaw-cn"] {
        let _ = Command::new(npm).args(["uninstall", "-g", package]).status();
    }
}

pub fn remove_openclaw_lockfiles() {
    for path in [
        config::state_dir().join("gateway.lock"),
        std::env::temp_dir().join("openclaw-gateway.lock"),
    ] {
        let _ = fs::remove_file(path);
    }
}

#[cfg(unix)]
fn chmod_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn chmod_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
unsafe fn libc_getuid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    getuid()
}

#[cfg(not(unix))]
unsafe fn libc_getuid() -> u32 {
    501
}

fn read_oneclaw_config() -> serde_json::Value {
    fs::read_to_string(config::oneclaw_config_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}))
}

fn write_oneclaw_preference(key: &str, value: impl serde::Serialize) -> Result<()> {
    let path = config::oneclaw_config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut current = read_oneclaw_config();
    current[key] = serde_json::to_value(value)?;
    fs::write(path, serde_json::to_string_pretty(&current)? + "\n")?;
    Ok(())
}

fn escape_for_posix_double_quoted(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        if matches!(ch, '"' | '\\' | '$' | '`') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

fn escape_for_cmd_set_value(value: &str) -> String {
    value.replace('"', "\"\"")
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &Path) -> Self {
            let prev = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(prev) = &self.prev {
                std::env::set_var(self.key, prev);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn detects_free_port_as_not_in_use() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(!is_port_in_use(port));
    }

    #[test]
    fn detects_bound_port_as_in_use() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(is_port_in_use(port));
    }

    #[test]
    fn ignores_oneclaw_cli_wrapper_when_detecting_global_command() {
        let dir = TempDir::new().unwrap();
        let wrapper = dir.path().join("openclaw");
        fs::write(&wrapper, format!("# {CLI_MARKER}\n")).unwrap();
        assert!(is_oneclaw_wrapper(&wrapper));
    }

    #[test]
    fn openclaw_wrapper_points_to_bundled_node_and_gateway_entry() {
        let wrapper = build_posix_openclaw_wrapper(Path::new("/tmp/node"), Path::new("/tmp/openclaw.mjs"));
        assert!(wrapper.contains(CLI_MARKER));
        assert!(wrapper.contains("APP_NODE=\"/tmp/node\""));
        assert!(wrapper.contains("APP_ENTRY=\"/tmp/openclaw.mjs\""));
    }

    #[test]
    #[serial(openclaw_env)]
    fn installs_cli_wrappers_into_state_bin() {
        let state = TempDir::new().unwrap();
        let resources = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let _state_guard = EnvGuard::set("OPENCLAW_STATE_DIR", state.path());
        let _resources_guard = EnvGuard::set("ONECLAW_RESOURCES_DIR", resources.path());
        let _home_guard = EnvGuard::set("HOME", home.path());

        let runtime = resources.path().join("runtime");
        let gateway = resources.path().join("gateway").join("node_modules");
        fs::create_dir_all(&runtime).unwrap();
        fs::create_dir_all(gateway.join("openclaw")).unwrap();
        fs::create_dir_all(gateway.join("clawhub").join("bin")).unwrap();
        fs::write(runtime.join(if cfg!(windows) { "node.exe" } else { "node" }), "").unwrap();
        fs::write(runtime.join(if cfg!(windows) { "npm.cmd" } else { "npm" }), "").unwrap();
        fs::write(gateway.join("openclaw").join("openclaw.mjs"), "").unwrap();
        fs::write(gateway.join("clawhub").join("bin").join("clawdhub.js"), "").unwrap();

        install_cli_best_effort().unwrap();
        let bin = state.path().join("bin");
        assert!(bin.join(if cfg!(windows) { "openclaw.cmd" } else { "openclaw" }).exists());
        assert!(bin.join(if cfg!(windows) { "clawhub.cmd" } else { "clawhub" }).exists());
    }

    #[test]
    fn rc_block_stripping_is_lossless_outside_managed_block() {
        let input = "before\n# >>> oneclaw-cli >>>\nmanaged\n# <<< oneclaw-cli <<<\nafter\n";
        let (stripped, removed) = strip_managed_rc_block(input);
        assert!(removed);
        assert_eq!(stripped, "before\nafter");
    }
}
