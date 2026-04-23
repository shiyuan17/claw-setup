use serde::Serialize;
use serde_json::Value;

use crate::config::{self, CompleteSetupParams, SaveConfigParams};
use crate::daemon;
use crate::oauth;
use crate::provider::{self, VerifyKeyParams};
use crate::system::{self, ConflictParams};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupResult<T = Value> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl<T> SetupResult<T> {
    pub fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            message: None,
        }
    }

    pub fn empty_ok() -> Self {
        Self {
            success: true,
            data: None,
            message: None,
        }
    }

    pub fn fail(message: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            message: Some(message.into()),
        }
    }
}

async fn run_blocking<T, F>(run: F) -> SetupResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> SetupResult<T> + Send + 'static,
{
    match tauri::async_runtime::spawn_blocking(run).await {
        Ok(result) => result,
        Err(err) => SetupResult::fail(format!("后台任务失败: {err}")),
    }
}

#[tauri::command]
pub fn detect_installation() -> SetupResult<system::DetectionResult> {
    match system::detect_existing_installation(system::DEFAULT_PORT) {
        Ok(result) => SetupResult::ok(result),
        Err(err) => SetupResult::fail(err.to_string()),
    }
}

#[tauri::command]
pub fn resolve_conflict(params: ConflictParams) -> SetupResult<Value> {
    match system::resolve_conflict(&params) {
        Ok(()) => SetupResult::empty_ok(),
        Err(err) => SetupResult::fail(err.to_string()),
    }
}

#[tauri::command]
pub fn setup_get_launch_at_login() -> SetupResult<system::LaunchAtLoginState> {
    SetupResult::ok(system::get_launch_at_login_state())
}

#[tauri::command]
pub fn verify_key(params: VerifyKeyParams) -> SetupResult<Value> {
    match provider::verify_provider(&params) {
        Ok(()) => SetupResult::empty_ok(),
        Err(err) => SetupResult::fail(provider::public_error(&err)),
    }
}

#[tauri::command]
pub fn save_config(params: SaveConfigParams) -> SetupResult<Value> {
    match config::save_setup_config(&params) {
        Ok(()) => SetupResult::empty_ok(),
        Err(err) => SetupResult::fail(err.to_string()),
    }
}

#[tauri::command]
pub async fn complete_setup(params: Option<CompleteSetupParams>) -> SetupResult<Value> {
    run_blocking(move || match config::complete_setup(params.as_ref()) {
        Ok(token) => match daemon::ensure_daemon_running(&token) {
            Ok(()) => SetupResult::empty_ok(),
            Err(err) => SetupResult::fail(err.to_string()),
        },
        Err(err) => SetupResult::fail(err.to_string()),
    })
    .await
}

#[tauri::command]
pub fn gateway_status() -> SetupResult<daemon::GatewayStatus> {
    SetupResult::ok(daemon::get_gateway_status())
}

#[tauri::command]
pub async fn restart_gateway() -> SetupResult<Value> {
    run_blocking(|| match daemon::restart_gateway() {
        Ok(()) => SetupResult::empty_ok(),
        Err(err) => SetupResult::fail(err.to_string()),
    })
    .await
}

#[tauri::command]
pub async fn test_openclaw_chat() -> SetupResult<daemon::ChatTestResult> {
    run_blocking(|| match daemon::test_openclaw_chat() {
        Ok(data) => SetupResult::ok(data),
        Err(err) => SetupResult::fail(err.to_string()),
    })
    .await
}

#[tauri::command]
pub fn kimi_oauth_login() -> SetupResult<oauth::OAuthLoginData> {
    match oauth::login() {
        Ok(data) => SetupResult::ok(data),
        Err(err) => SetupResult::fail(provider::public_error(&err)),
    }
}

#[tauri::command]
pub fn kimi_oauth_cancel() -> SetupResult<Value> {
    oauth::cancel();
    SetupResult::empty_ok()
}

#[tauri::command]
pub fn kimi_oauth_logout() -> SetupResult<Value> {
    match oauth::logout() {
        Ok(()) => SetupResult::empty_ok(),
        Err(err) => SetupResult::fail(err.to_string()),
    }
}

#[tauri::command]
pub fn kimi_oauth_status() -> SetupResult<oauth::OAuthStatus> {
    SetupResult::ok(oauth::status())
}

#[tauri::command]
pub fn open_external(url: String) -> SetupResult<Value> {
    match open::that(url) {
        Ok(()) => SetupResult::empty_ok(),
        Err(err) => SetupResult::fail(err.to_string()),
    }
}
