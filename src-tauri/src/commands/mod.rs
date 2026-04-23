use serde::Serialize;
use serde_json::Value;

use crate::config::{self, CompleteSetupParams, SaveConfigParams};
use crate::daemon;
use crate::logging;
use crate::oauth;
use crate::provider::{self, VerifyKeyParams};
use crate::proxy;
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
pub fn verify_key(mut params: VerifyKeyParams) -> SetupResult<Value> {
    if params.provider == "moonshot" && params.sub_platform.as_deref() == Some("kimi-code") {
        let Some(api_key) = params.api_key.as_deref().map(str::trim).filter(|value| !value.is_empty()) else {
            return SetupResult::fail("API Key 不能为空");
        };
        match proxy::start_auth_proxy(params.proxy_port, Some(system::DEFAULT_PORT), api_key, oauth::load_search_dedicated_key()) {
            Ok(port) => {
                proxy::set_access_token(api_key.to_string());
                params.proxy_port = Some(port);
            }
            Err(err) => return SetupResult::fail(err.to_string()),
        }
    }

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
    run_blocking(move || match config::prepare_setup_completion(params.as_ref()) {
        Ok(token) => {
            if let Some(launch_at_login) = params.as_ref().and_then(|value| value.launch_at_login) {
                if let Err(err) = system::set_launch_at_login_enabled(launch_at_login) {
                    logging::warn(format!("设置开机启动失败: {err}"));
                }
            }

            if let Err(err) = daemon::ensure_daemon_running(&token) {
                return SetupResult::fail(err.to_string());
            }

            if let Err(err) = config::finalize_setup_completion() {
                return SetupResult::fail(err.to_string());
            }

            if params.as_ref().and_then(|value| value.install_cli).unwrap_or(true) {
                if let Err(err) = system::install_cli_best_effort() {
                    logging::warn(format!("CLI 安装失败: {err}"));
                }
            } else if let Err(err) = system::uninstall_cli_best_effort() {
                logging::warn(format!("CLI 卸载失败: {err}"));
            }

            SetupResult::empty_ok()
        }
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
