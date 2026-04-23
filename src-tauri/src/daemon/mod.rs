use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tungstenite::client::IntoClientRequest;
use tungstenite::{Message, WebSocket};

use crate::config;
use crate::oauth;
use crate::proxy;
use crate::runtime;
use crate::system::{self, DEFAULT_PORT};

const HEALTH_TIMEOUT: Duration = Duration::from_secs(90);
const HEALTH_INTERVAL: Duration = Duration::from_millis(500);
const RPC_TIMEOUT: Duration = Duration::from_secs(60);
const RPC_POLL_INTERVAL: Duration = Duration::from_secs(1);
const TEST_SESSION_KEY: &str = "oneclaw-setup-smoke-test";
const TEST_CHAT_PROMPT: &str = "请简短回复：OneClaw setup chat test ok";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DaemonState {
    pub daemon_pid: u32,
    pub gateway_pid: u32,
    pub gateway_port: u16,
    pub proxy_port: Option<u16>,
    pub started_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatus {
    pub state: String,
    pub gateway_port: u16,
    pub daemon_pid: Option<u32>,
    pub gateway_pid: Option<u32>,
    pub proxy_port: Option<u16>,
    pub started_at: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChatTestResult {
    pub reply: String,
    pub session_key: String,
}

pub fn ensure_daemon_running(token: &str) -> Result<()> {
    if probe_gateway(DEFAULT_PORT) {
        write_daemon_state(&DaemonState {
            daemon_pid: std::process::id(),
            gateway_pid: 0,
            gateway_port: DEFAULT_PORT,
            proxy_port: None,
            started_at: Utc::now().to_rfc3339(),
        })?;
        return Ok(());
    }

    let exe = std::env::current_exe().context("无法定位 claw-setup 可执行文件")?;
    let mut command = Command::new(exe);
    command.arg("--daemon");
    command.env("OPENCLAW_GATEWAY_TOKEN", token);
    if let Ok(state_dir) = std::env::var("OPENCLAW_STATE_DIR") {
        command.env("OPENCLAW_STATE_DIR", state_dir);
    }
    if let Ok(resources_dir) = std::env::var("ONECLAW_RESOURCES_DIR") {
        command.env("ONECLAW_RESOURCES_DIR", resources_dir);
    }
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    command.spawn().context("启动 claw-setup daemon 失败")?;

    wait_for_gateway(DEFAULT_PORT, HEALTH_TIMEOUT)
}

pub fn get_gateway_status() -> GatewayStatus {
    let state = read_daemon_state().ok().flatten();
    let running = probe_gateway(DEFAULT_PORT);
    let gateway_alive = state
        .as_ref()
        .and_then(|value| (value.gateway_pid > 0).then_some(value.gateway_pid))
        .is_some_and(system::is_pid_running);
    let daemon_alive = state
        .as_ref()
        .and_then(|value| (value.daemon_pid > 0).then_some(value.daemon_pid))
        .is_some_and(system::is_pid_running);
    let (status, message) = classify_gateway_state(running, state.as_ref(), gateway_alive, daemon_alive);

    GatewayStatus {
        state: status.to_string(),
        gateway_port: state.as_ref().map(|value| value.gateway_port).unwrap_or(DEFAULT_PORT),
        daemon_pid: state.as_ref().and_then(|value| (value.daemon_pid > 0).then_some(value.daemon_pid)),
        gateway_pid: state.as_ref().and_then(|value| (value.gateway_pid > 0).then_some(value.gateway_pid)),
        proxy_port: state.as_ref().and_then(|value| value.proxy_port),
        started_at: state.as_ref().map(|value| value.started_at.clone()),
        message: message.map(ToOwned::to_owned),
    }
}

pub fn restart_gateway() -> Result<()> {
    let token = ensure_current_gateway_token()?;
    stop_existing_gateway(&token);
    ensure_daemon_running(&token)
}

pub fn test_openclaw_chat() -> Result<ChatTestResult> {
    let token = ensure_current_gateway_token()?;
    ensure_daemon_running(&token)?;

    let mut socket = GatewayRpcSocket::connect(&token)?;
    let session_key = TEST_SESSION_KEY.to_string();
    let run_id = rpc_id();
    let request_id = socket.send_request(
        "chat.send",
        json!({
            "sessionKey": session_key,
            "message": TEST_CHAT_PROMPT,
            "deliver": false,
            "idempotencyKey": run_id,
        }),
    )?;
    let deadline = Instant::now() + RPC_TIMEOUT;
    let result = wait_for_chat_reply(&mut socket, &request_id, &session_key, &run_id, deadline);
    let _ = cleanup_test_session(&token, &session_key);
    let reply = result?;
    Ok(ChatTestResult { reply, session_key })
}

pub fn run_daemon() -> Result<()> {
    let token = std::env::var("OPENCLAW_GATEWAY_TOKEN").unwrap_or_default();
    let token = if token.trim().is_empty() {
        let mut config = config::read_user_config();
        config::ensure_gateway_auth_token_in_config(&mut config)
    } else {
        token
    };

    let proxy_port = ensure_auth_proxy()?;
    let child = spawn_gateway(&token)?;
    write_daemon_state(&DaemonState {
        daemon_pid: std::process::id(),
        gateway_pid: child,
        gateway_port: DEFAULT_PORT,
        proxy_port,
        started_at: Utc::now().to_rfc3339(),
    })?;
    wait_for_gateway(DEFAULT_PORT, HEALTH_TIMEOUT)?;
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}

pub fn spawn_gateway(token: &str) -> Result<u32> {
    let layout = runtime::resolve_runtime_layout()?;
    runtime::validate_runtime_layout(&layout)?;

    let log_path = config::state_dir().join("gateway.log");
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let stdout = OpenOptions::new().create(true).append(true).open(&log_path)?;
    let stderr = OpenOptions::new().create(true).append(true).open(&log_path)?;

    let mut command = Command::new(&layout.node_bin);
    apply_gateway_env(&mut command, &layout, token)?;
    let child = command
        .arg(&layout.gateway_entry)
        .arg("gateway")
        .arg("run")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .context("启动 openclaw gateway 失败")?;
    Ok(child.id())
}

fn ensure_auth_proxy() -> Result<Option<u16>> {
    let mut config = config::read_user_config();
    let provider = config
        .get("models")
        .and_then(|value| value.get("providers"))
        .and_then(|value| value.get("kimi-coding"))
        .cloned();
    let Some(provider) = provider else {
        return Ok(None);
    };

    let provider_api_key = provider
        .get("apiKey")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    let oauth_token = oauth::load_token();
    let access_token = oauth_token
        .as_ref()
        .map(|token| token.access_token.clone())
        .or_else(oauth::load_manual_kimi_api_key)
        .or_else(|| {
            (provider_api_key != "proxy-managed" && !provider_api_key.is_empty()).then_some(provider_api_key.clone())
        })
        .unwrap_or_default();
    if access_token.trim().is_empty() {
        return Ok(None);
    }

    if oauth_token.is_none() && provider_api_key != "proxy-managed" && !provider_api_key.is_empty() {
        oauth::save_manual_kimi_api_key(&provider_api_key)?;
    }

    let preferred = provider
        .get("baseUrl")
        .and_then(|value| value.as_str())
        .and_then(parse_local_port);
    let search_key = oauth::load_search_dedicated_key();
    let proxy_port = proxy::start_auth_proxy(preferred, Some(DEFAULT_PORT), access_token, search_key)?;
    ensure_proxy_config(&mut config, proxy_port)?;

    if oauth_token.is_some() {
        oauth::spawn_token_refresh_loop(|token| {
            proxy::set_access_token(token.access_token);
        });
    }

    Ok(Some(proxy_port))
}

fn ensure_proxy_config(config: &mut serde_json::Value, proxy_port: u16) -> Result<()> {
    let expected_base = format!("http://127.0.0.1:{proxy_port}/coding");
    if let Some(provider) = config
        .get_mut("models")
        .and_then(|value| value.get_mut("providers"))
        .and_then(|value| value.get_mut("kimi-coding"))
        .and_then(|value| value.as_object_mut())
    {
        provider.insert("baseUrl".to_string(), serde_json::Value::String(expected_base));
        provider.insert("apiKey".to_string(), serde_json::Value::String("proxy-managed".to_string()));
    }

    ensure_memory_search_proxy_config(config, proxy_port);
    if let Some(search_entry) = config
        .get_mut("plugins")
        .and_then(|value| value.get_mut("entries"))
        .and_then(|value| value.get_mut("kimi-search"))
        .and_then(|value| value.as_object_mut())
    {
        let config_entry = search_entry
            .entry("config")
            .or_insert_with(|| serde_json::json!({}));
        if !config_entry.is_object() {
            *config_entry = serde_json::json!({});
        }
        config_entry["search"] =
            serde_json::json!({ "baseUrl": format!("http://127.0.0.1:{proxy_port}/coding/v1/search") });
        config_entry["fetch"] =
            serde_json::json!({ "baseUrl": format!("http://127.0.0.1:{proxy_port}/coding/v1/fetch") });
    }

    config::write_user_config(config)
}

fn ensure_memory_search_proxy_config(config: &mut serde_json::Value, proxy_port: u16) {
    ensure_object_path(config, &["agents", "defaults", "memorySearch"]);
    config["agents"]["defaults"]["memorySearch"]["enabled"] = serde_json::Value::Bool(true);
    config["agents"]["defaults"]["memorySearch"]["provider"] = serde_json::Value::String("openai".to_string());
    config["agents"]["defaults"]["memorySearch"]["model"] = serde_json::Value::String("bge_m3_embed".to_string());
    ensure_object_path(config, &["agents", "defaults", "memorySearch", "remote"]);
    config["agents"]["defaults"]["memorySearch"]["remote"]["baseUrl"] =
        serde_json::Value::String(format!("http://127.0.0.1:{proxy_port}/coding/v1/"));
    config["agents"]["defaults"]["memorySearch"]["remote"]["apiKey"] =
        serde_json::Value::String("proxy-managed".to_string());
}

fn ensure_object_path<'a>(root: &'a mut serde_json::Value, path: &[&str]) -> &'a mut serde_json::Map<String, serde_json::Value> {
    if !root.is_object() {
        *root = serde_json::json!({});
    }
    let mut current = root;
    for key in path {
        let object = current.as_object_mut().expect("object ensured");
        current = object
            .entry((*key).to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !current.is_object() {
            *current = serde_json::json!({});
        }
    }
    current.as_object_mut().expect("object ensured")
}

fn parse_local_port(base_url: &str) -> Option<u16> {
    let parsed = reqwest::Url::parse(base_url).ok()?;
    match parsed.host_str() {
        Some("127.0.0.1") | Some("localhost") => parsed.port(),
        _ => None,
    }
}

pub fn wait_for_gateway(port: u16, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if probe_gateway(port) {
            return Ok(());
        }
        thread::sleep(HEALTH_INTERVAL);
    }
    Err(anyhow!("Gateway 启动超时或失败，请稍后重试。"))
}

pub fn probe_gateway(port: u16) -> bool {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .and_then(|client| client.get(format!("http://127.0.0.1:{port}/")).send())
        .map(|response| response.status().as_u16() == 200)
        .unwrap_or(false)
}

pub fn write_daemon_state(state: &DaemonState) -> Result<()> {
    let path = config::daemon_state_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(state)? + "\n")?;
    Ok(())
}

fn ensure_current_gateway_token() -> Result<String> {
    let mut config = config::read_user_config();
    let token = config::ensure_gateway_auth_token_in_config(&mut config);
    config::write_user_config(&config)?;
    Ok(token)
}

fn cleanup_test_session(token: &str, session_key: &str) -> Result<()> {
    let mut socket = GatewayRpcSocket::connect(token)?;
    let request_id = socket.send_request(
        "sessions.delete",
        json!({
            "key": session_key,
            "deleteTranscript": true,
        }),
    )?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let Some(frame) = socket.read_json(deadline)? else {
            continue;
        };
        if frame.get("type").and_then(Value::as_str) != Some("res") {
            continue;
        }
        if frame.get("id").and_then(Value::as_str) != Some(request_id.as_str()) {
            continue;
        }
        return Ok(());
    }
    Ok(())
}

fn wait_for_chat_reply(
    socket: &mut GatewayRpcSocket,
    request_id: &str,
    session_key: &str,
    run_id: &str,
    deadline: Instant,
) -> Result<String> {
    let mut reply = String::new();
    let mut send_confirmed = false;
    while Instant::now() < deadline {
        let Some(frame) = socket.read_json(deadline)? else {
            continue;
        };
        match frame.get("type").and_then(Value::as_str) {
            Some("res") if frame.get("id").and_then(Value::as_str) == Some(request_id) => {
                let ok = frame.get("ok").and_then(Value::as_bool).unwrap_or(false);
                if !ok {
                    let message = frame
                        .get("error")
                        .and_then(|value| value.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("聊天测试发送失败");
                    return Err(anyhow!("{message}"));
                }
                send_confirmed = true;
            }
            Some("event") if frame.get("event").and_then(Value::as_str) == Some("chat") => {
                let Some(payload) = frame.get("payload") else {
                    continue;
                };
                if payload.get("sessionKey").and_then(Value::as_str) != Some(session_key) {
                    continue;
                }
                if payload.get("runId").and_then(Value::as_str) != Some(run_id) {
                    continue;
                }
                if payload.get("state").and_then(Value::as_str) == Some("error") {
                    let message = payload
                        .get("errorMessage")
                        .and_then(Value::as_str)
                        .unwrap_or("聊天测试失败");
                    return Err(anyhow!("{message}"));
                }
                if let Some(text) = payload.get("message").and_then(extract_text_from_message) {
                    if reply.is_empty() {
                        reply = text;
                    } else if !text.is_empty() {
                        reply.push_str(&text);
                    }
                }
                if payload.get("state").and_then(Value::as_str) == Some("final") {
                    if reply.trim().is_empty() {
                        return Err(anyhow!("Gateway 已响应，但未收到有效聊天回复"));
                    }
                    return Ok(reply.trim().to_string());
                }
            }
            _ => {}
        }
    }

    if !send_confirmed {
        return Err(anyhow!("发送测试消息超时"));
    }
    Err(anyhow!("等待聊天回复超时"))
}

fn extract_text_from_message(message: &Value) -> Option<String> {
    if let Some(text) = message.get("content").and_then(Value::as_str) {
        let trimmed = text.trim();
        return (!trimmed.is_empty()).then(|| trimmed.to_string());
    }

    if let Some(items) = message.get("content").and_then(Value::as_array) {
        let joined = items
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if !joined.trim().is_empty() {
            return Some(joined);
        }
    }

    message
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

fn classify_gateway_state(
    running: bool,
    state: Option<&DaemonState>,
    gateway_alive: bool,
    daemon_alive: bool,
) -> (&'static str, Option<&'static str>) {
    if running {
        return ("running", None);
    }
    if state.is_none() {
        return ("stopped", None);
    }
    if gateway_alive || daemon_alive {
        return ("starting", Some("Gateway 正在启动"));
    }
    ("error", Some("检测到残留 daemon 状态，但 Gateway 未响应"))
}

fn read_daemon_state() -> Result<Option<DaemonState>> {
    let path = config::daemon_state_path();
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path)?;
    Ok(serde_json::from_str::<DaemonState>(&raw).ok())
}

fn remove_daemon_state_file() {
    let _ = fs::remove_file(config::daemon_state_path());
}

fn stop_existing_gateway(token: &str) {
    let state = read_daemon_state().ok().flatten();
    let _ = stop_gateway_via_cli(token);

    if let Some(daemon_pid) = state
        .as_ref()
        .and_then(|value| (value.daemon_pid > 0).then_some(value.daemon_pid))
        .filter(|pid| *pid != std::process::id())
    {
        let _ = system::kill_pid_force(daemon_pid);
    }

    if !wait_for_port_release(Duration::from_secs(8)) {
        if let Some(pid) = system::detect_port_pid(DEFAULT_PORT) {
            let _ = system::kill_pid_force(pid);
        }
        if let Some(gateway_pid) = state
            .as_ref()
            .and_then(|value| (value.gateway_pid > 0).then_some(value.gateway_pid))
        {
            let _ = system::kill_pid_force(gateway_pid);
        }
        let _ = wait_for_port_release(Duration::from_secs(4));
    }

    remove_daemon_state_file();
}

fn stop_gateway_via_cli(token: &str) -> Result<()> {
    let layout = runtime::resolve_runtime_layout()?;
    runtime::validate_runtime_layout(&layout)?;
    let mut command = Command::new(&layout.node_bin);
    apply_gateway_env(&mut command, &layout, token)?;
    let status = command
        .arg(&layout.gateway_entry)
        .arg("gateway")
        .arg("stop")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("停止 openclaw gateway 失败")?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("停止 openclaw gateway 失败"))
    }
}

fn wait_for_port_release(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !probe_gateway(DEFAULT_PORT) && system::detect_port_pid(DEFAULT_PORT).is_none() {
            return true;
        }
        thread::sleep(HEALTH_INTERVAL);
    }
    false
}

fn apply_gateway_env(command: &mut Command, layout: &runtime::RuntimeLayout, token: &str) -> Result<()> {
    let state_dir = config::state_dir();
    command
        .current_dir(&layout.gateway_cwd)
        .env("NODE_ENV", "production")
        .env("OPENCLAW_NO_RESPAWN", "1")
        .env("OPENCLAW_LENIENT_CONFIG", "1")
        .env("OPENCLAW_STATE_DIR", &state_dir)
        .env("OPENCLAW_INSTALL_ROOT", &layout.resources_dir)
        .env("OPENCLAW_GATEWAY_TOKEN", token)
        .env("OPENCLAW_NPM_BIN", &layout.npm_bin)
        .env("PATH", build_env_path(layout)?);
    Ok(())
}

fn build_env_path(layout: &runtime::RuntimeLayout) -> Result<OsString> {
    let user_bin_dir = config::state_dir().join("bin");
    let runtime_dir = layout.resources_dir.join("runtime");
    let existing_path = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths([user_bin_dir.as_os_str(), runtime_dir.as_os_str(), existing_path.as_os_str()])
        .map_err(|err| anyhow!("构造 PATH 失败: {err}"))
}

fn rpc_id() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

struct GatewayRpcSocket {
    socket: WebSocket<TcpStream>,
}

impl GatewayRpcSocket {
    fn connect(token: &str) -> Result<Self> {
        let address = format!("127.0.0.1:{DEFAULT_PORT}");
        let stream = TcpStream::connect(&address)
            .with_context(|| format!("连接 Gateway 失败: {address}"))?;
        stream.set_read_timeout(Some(RPC_POLL_INTERVAL))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        let request = format!("ws://127.0.0.1:{DEFAULT_PORT}/")
            .into_client_request()
            .context("构造 Gateway WebSocket 请求失败")?;
        let (mut socket, _) =
            tungstenite::client(request, stream).context("打开 Gateway WebSocket 失败")?;

        let connect_id = rpc_id();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut connect_sent = false;

        while Instant::now() < deadline {
            let Some(frame) = Self::read_json_frame(&mut socket, deadline)? else {
                continue;
            };
            match frame.get("type").and_then(Value::as_str) {
                Some("event") if frame.get("event").and_then(Value::as_str) == Some("connect.challenge") => {
                    if connect_sent {
                        continue;
                    }
                    let payload = json!({
                        "type": "req",
                        "id": connect_id,
                        "method": "connect",
                        "params": {
                            "minProtocol": 3,
                            "maxProtocol": 3,
                            "client": {
                                "id": "claw-setup",
                                "displayName": "Claw Setup",
                                "version": env!("CARGO_PKG_VERSION"),
                                "platform": std::env::consts::OS,
                                "mode": "backend",
                            },
                            "auth": { "token": token },
                            "role": "operator",
                            "scopes": ["operator.admin"],
                        }
                    });
                    socket
                        .send(Message::Text(payload.to_string()))
                        .context("发送 Gateway connect 请求失败")?;
                    connect_sent = true;
                }
                Some("res") if frame.get("id").and_then(Value::as_str) == Some(connect_id.as_str()) => {
                    let ok = frame.get("ok").and_then(Value::as_bool).unwrap_or(false);
                    if ok {
                        return Ok(Self { socket });
                    }
                    let message = frame
                        .get("error")
                        .and_then(|value| value.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("Gateway 鉴权失败");
                    return Err(anyhow!("{message}"));
                }
                _ => {}
            }
        }

        Err(anyhow!("Gateway WebSocket 握手超时"))
    }

    fn send_request(&mut self, method: &str, params: Value) -> Result<String> {
        let id = rpc_id();
        let payload = json!({
            "type": "req",
            "id": id,
            "method": method,
            "params": params,
        });
        self.socket
            .send(Message::Text(payload.to_string()))
            .with_context(|| format!("发送 Gateway 请求失败: {method}"))?;
        Ok(id)
    }

    fn read_json(&mut self, deadline: Instant) -> Result<Option<Value>> {
        Self::read_json_frame(&mut self.socket, deadline)
    }

    fn read_json_frame(socket: &mut WebSocket<TcpStream>, deadline: Instant) -> Result<Option<Value>> {
        while Instant::now() < deadline {
            match socket.read() {
                Ok(Message::Text(text)) => {
                    let frame = serde_json::from_str::<Value>(&text)
                        .with_context(|| format!("解析 Gateway 消息失败: {text}"))?;
                    return Ok(Some(frame));
                }
                Ok(Message::Binary(bytes)) => {
                    let raw = String::from_utf8(bytes)
                        .context("Gateway 返回了无效 UTF-8 二进制消息")?;
                    let frame = serde_json::from_str::<Value>(&raw)
                        .with_context(|| format!("解析 Gateway 消息失败: {raw}"))?;
                    return Ok(Some(frame));
                }
                Ok(Message::Ping(payload)) => {
                    socket.send(Message::Pong(payload))?;
                }
                Ok(Message::Pong(_)) => {}
                Ok(Message::Close(_)) => return Err(anyhow!("Gateway WebSocket 已关闭")),
                Ok(Message::Frame(_)) => {}
                Err(tungstenite::Error::Io(err))
                    if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(err) => return Err(anyhow!("读取 Gateway WebSocket 失败: {err}")),
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    struct EnvGuard {
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(path: &std::path::Path) -> Self {
            let prev = std::env::var("OPENCLAW_STATE_DIR").ok();
            std::env::set_var("OPENCLAW_STATE_DIR", path);
            Self { prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(prev) = &self.prev {
                std::env::set_var("OPENCLAW_STATE_DIR", prev);
            } else {
                std::env::remove_var("OPENCLAW_STATE_DIR");
            }
        }
    }

    #[test]
    #[serial(openclaw_env)]
    fn writes_daemon_state_to_openclaw_state_dir() {
        let dir = TempDir::new().unwrap();
        let _guard = EnvGuard::set(dir.path());
        write_daemon_state(&DaemonState {
            daemon_pid: 1,
            gateway_pid: 2,
            gateway_port: 18789,
            proxy_port: Some(19000),
            started_at: "2026-01-01T00:00:00Z".to_string(),
        })
        .unwrap();
        let raw = fs::read_to_string(dir.path().join("claw-setup-daemon.json")).unwrap();
        assert!(raw.contains("\"gatewayPort\": 18789"));
    }

    #[test]
    fn probe_gateway_requires_http_200() {
        let server = tiny_http::Server::http(("127.0.0.1", 0)).unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        std::thread::spawn(move || {
            if let Ok(request) = server.recv() {
                let _ = request.respond(tiny_http::Response::from_string("ok"));
            }
        });
        assert!(probe_gateway(port));
    }

    #[test]
    fn parses_existing_local_proxy_port_from_config_url() {
        assert_eq!(parse_local_port("http://127.0.0.1:18790/coding"), Some(18790));
        assert_eq!(parse_local_port("https://api.kimi.com/coding"), None);
    }

    #[test]
    fn classifies_gateway_state_from_probe_and_pids() {
        let state = DaemonState {
            daemon_pid: 11,
            gateway_pid: 22,
            gateway_port: DEFAULT_PORT,
            proxy_port: None,
            started_at: "2026-01-01T00:00:00Z".to_string(),
        };
        assert_eq!(
            classify_gateway_state(true, Some(&state), false, false),
            ("running", None)
        );
        assert_eq!(
            classify_gateway_state(false, Some(&state), true, false),
            ("starting", Some("Gateway 正在启动"))
        );
        assert_eq!(
            classify_gateway_state(false, Some(&state), false, false),
            ("error", Some("检测到残留 daemon 状态，但 Gateway 未响应"))
        );
        assert_eq!(classify_gateway_state(false, None, false, false), ("stopped", None));
    }

    #[test]
    fn extracts_text_from_gateway_message_payload() {
        assert_eq!(
            extract_text_from_message(&json!({
                "content": [{ "type": "text", "text": "hello" }]
            })),
            Some("hello".to_string())
        );
        assert_eq!(
            extract_text_from_message(&json!({
                "text": "hi there"
            })),
            Some("hi there".to_string())
        );
    }
}
