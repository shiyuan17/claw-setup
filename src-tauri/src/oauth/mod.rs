use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

use crate::config;

const KIMI_CODE_CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
const OAUTH_BASE: &str = "https://auth.kimi.com";
const POLL_MAX_RETRIES: usize = 120;
const REFRESH_CHECK_INTERVAL: Duration = Duration::from_secs(60);
const REFRESH_THRESHOLD_SECS: i64 = 300;

static ABORT_FLAG: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OAuthToken {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub scope: String,
    pub token_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthLoginData {
    pub access_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OAuthStatus {
    pub logged_in: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct DeviceAuthResponse {
    device_code: String,
    verification_uri_complete: String,
    interval: u64,
}

pub fn login() -> Result<OAuthLoginData> {
    ABORT_FLAG.store(false, Ordering::SeqCst);
    let client = oauth_client()?;
    let auth: DeviceAuthResponse = post_form(
        &client,
        "/api/oauth/device_authorization",
        &[("client_id", KIMI_CODE_CLIENT_ID)],
    )?;
    open::that(auth.verification_uri_complete).context("打开 Kimi 授权页面失败")?;
    let token = poll_for_token(&client, &auth.device_code, auth.interval.max(1))?;
    save_token(&token)?;
    Ok(OAuthLoginData {
        access_token: token.access_token,
    })
}

pub fn cancel() {
    ABORT_FLAG.store(true, Ordering::SeqCst);
}

pub fn logout() -> Result<()> {
    let path = token_path();
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

pub fn status() -> OAuthStatus {
    match load_token() {
        Some(token) => OAuthStatus {
            logged_in: true,
            expires_at: Some(token.expires_at),
        },
        None => OAuthStatus {
            logged_in: false,
            expires_at: None,
        },
    }
}

pub fn load_token() -> Option<OAuthToken> {
    fs::read_to_string(token_path())
        .ok()
        .and_then(|raw| serde_json::from_str::<OAuthToken>(&raw).ok())
}

pub fn save_token(token: &OAuthToken) -> Result<()> {
    let path = token_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_string_pretty(token)? + "\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

pub fn token_path() -> std::path::PathBuf {
    config::state_dir()
        .join("credentials")
        .join("kimi-oauth-token.json")
}

pub fn manual_kimi_api_key_path() -> std::path::PathBuf {
    config::state_dir().join("credentials").join("kimi-api-key")
}

pub fn search_dedicated_key_path() -> std::path::PathBuf {
    config::state_dir()
        .join("credentials")
        .join("kimi-search-api-key")
}

pub fn load_manual_kimi_api_key() -> Option<String> {
    read_sidecar_value(manual_kimi_api_key_path())
}

pub fn save_manual_kimi_api_key(value: &str) -> Result<()> {
    write_sidecar_value(manual_kimi_api_key_path(), value)
}

pub fn load_search_dedicated_key() -> Option<String> {
    read_sidecar_value(search_dedicated_key_path())
}

pub fn refresh_token(token: &OAuthToken) -> Result<OAuthToken> {
    let client = oauth_client()?;
    let data: serde_json::Value = post_form(
        &client,
        "/api/oauth/token",
        &[
            ("client_id", KIMI_CODE_CLIENT_ID),
            ("grant_type", "refresh_token"),
            ("refresh_token", token.refresh_token.as_str()),
        ],
    )?;
    let refreshed = parse_token(data)?;
    save_token(&refreshed)?;
    Ok(refreshed)
}

pub fn spawn_token_refresh_loop(on_refreshed: impl Fn(OAuthToken) + Send + 'static) {
    std::thread::spawn(move || loop {
        std::thread::sleep(REFRESH_CHECK_INTERVAL);
        let Some(token) = load_token() else {
            continue;
        };
        if token.expires_at - now_epoch() >= REFRESH_THRESHOLD_SECS {
            continue;
        }
        if let Ok(refreshed) = refresh_token(&token) {
            on_refreshed(refreshed);
        }
    });
}

fn poll_for_token(client: &Client, device_code: &str, interval: u64) -> Result<OAuthToken> {
    for _ in 0..POLL_MAX_RETRIES {
        std::thread::sleep(Duration::from_secs(interval));
        if ABORT_FLAG.load(Ordering::SeqCst) {
            return Err(anyhow!("已取消"));
        }

        let data: serde_json::Value = post_form(
            client,
            "/api/oauth/token",
            &[
                ("client_id", KIMI_CODE_CLIENT_ID),
                ("device_code", device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ],
        )?;
        if data.get("access_token").and_then(|value| value.as_str()).is_some() {
            return parse_token(data);
        }
        match data.get("error").and_then(|value| value.as_str()) {
            Some("authorization_pending") | Some("slow_down") | None => {}
            Some("expired_token") => return Err(anyhow!("授权已过期，请重新登录")),
            Some(error) => return Err(anyhow!("OAuth 错误: {error}")),
        }
    }
    Err(anyhow!("轮询超时，请重新登录"))
}

fn parse_token(data: serde_json::Value) -> Result<OAuthToken> {
    let expires_in = data.get("expires_in").and_then(|value| value.as_i64()).unwrap_or(3600);
    Ok(OAuthToken {
        access_token: data
            .get("access_token")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("OAuth 响应缺少 access_token"))?
            .to_string(),
        refresh_token: data
            .get("refresh_token")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        expires_at: now_epoch() + expires_in,
        scope: data.get("scope").and_then(|value| value.as_str()).unwrap_or_default().to_string(),
        token_type: data
            .get("token_type")
            .and_then(|value| value.as_str())
            .unwrap_or("Bearer")
            .to_string(),
    })
}

fn post_form<T: serde::de::DeserializeOwned>(client: &Client, path: &str, form: &[(&str, &str)]) -> Result<T> {
    let response = client
        .post(format!("{OAUTH_BASE}{path}"))
        .header("X-Msh-Platform", "oneclaw")
        .form(form)
        .send()
        .context("网络错误")?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!("OAuth 请求失败 ({})", status.as_u16()));
    }
    response.json::<T>().context("OAuth 响应解析失败")
}

fn oauth_client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("创建 OAuth HTTP client 失败")
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn read_sidecar_value(path: std::path::PathBuf) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn write_sidecar_value(path: std::path::PathBuf, value: &str) -> Result<()> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        let _ = fs::remove_file(path);
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, trimmed)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
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
    fn saves_and_loads_oauth_token_in_credentials_dir() {
        let dir = TempDir::new().unwrap();
        let _guard = EnvGuard::set(dir.path());
        let token = OAuthToken {
            access_token: "access".to_string(),
            refresh_token: "refresh".to_string(),
            expires_at: 123,
            scope: "scope".to_string(),
            token_type: "Bearer".to_string(),
        };
        save_token(&token).unwrap();
        assert_eq!(load_token(), Some(token));
        assert!(token_path().starts_with(dir.path()));
    }

    #[test]
    #[serial(openclaw_env)]
    fn status_reports_logged_out_without_token() {
        let dir = TempDir::new().unwrap();
        let _guard = EnvGuard::set(dir.path());
        assert_eq!(
            status(),
            OAuthStatus {
                logged_in: false,
                expires_at: None
            }
        );
    }

    #[test]
    #[serial(openclaw_env)]
    fn saves_and_loads_manual_kimi_api_key_sidecar() {
        let dir = TempDir::new().unwrap();
        let _guard = EnvGuard::set(dir.path());
        save_manual_kimi_api_key("kimi-key").unwrap();
        assert_eq!(load_manual_kimi_api_key().as_deref(), Some("kimi-key"));
    }
}
