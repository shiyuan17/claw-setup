use std::fs;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::config;

pub const DEFAULT_PORT: u16 = 18_789;
const CLI_MARKER: &str = "OneClaw CLI";

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
        enabled: read_cli_preference("launchAtLogin").unwrap_or(false),
    }
}

pub fn set_launch_at_login_enabled(enabled: bool) -> Result<()> {
    write_oneclaw_preference("launchAtLogin", enabled)
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
    write_oneclaw_preference("cliPreference", "installed")
}

pub fn uninstall_cli_best_effort() -> Result<()> {
    write_oneclaw_preference("cliPreference", "uninstalled")
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

fn is_oneclaw_wrapper(path: &PathBuf) -> bool {
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

fn uninstall_gateway_daemon_best_effort() {
    if cfg!(target_os = "macos") {
        let uid = unsafe { libc_getuid() };
        let domain = format!("gui/{uid}");
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
}

fn uninstall_global_openclaw_best_effort() {
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    for package in ["openclaw", "openclaw-cn"] {
        let _ = Command::new(npm).args(["uninstall", "-g", package]).status();
    }
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

fn read_cli_preference(key: &str) -> Option<bool> {
    let raw = fs::read_to_string(config::oneclaw_config_path()).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value.get(key)?.as_bool()
}

fn write_oneclaw_preference(key: &str, value: impl serde::Serialize) -> Result<()> {
    let path = config::oneclaw_config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut current: serde_json::Value = fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !current.is_object() {
        current = serde_json::json!({});
    }
    current[key] = serde_json::to_value(value)?;
    fs::write(path, serde_json::to_string_pretty(&current)? + "\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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
}
