use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::provider::{self, SaveConfigParamsLike};
use crate::system;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveConfigParams {
    pub provider: String,
    pub api_key: String,
    #[serde(rename = "modelID", alias = "modelId")]
    pub model_id: String,
    #[serde(rename = "baseURL", alias = "baseUrl")]
    pub base_url: Option<String>,
    pub api: Option<String>,
    pub sub_platform: Option<String>,
    pub support_image: Option<bool>,
    pub custom_preset: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteSetupParams {
    pub install_cli: Option<bool>,
    pub launch_at_login: Option<bool>,
    pub session_memory: Option<bool>,
}

pub fn state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("OPENCLAW_STATE_DIR") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".openclaw")
}

pub fn user_config_path() -> PathBuf {
    state_dir().join("openclaw.json")
}

pub fn oneclaw_config_path() -> PathBuf {
    state_dir().join("oneclaw.config.json")
}

pub fn daemon_state_path() -> PathBuf {
    state_dir().join("claw-setup-daemon.json")
}

pub fn read_user_config() -> Value {
    read_json_file(&user_config_path()).unwrap_or_else(|| json!({}))
}

pub fn save_setup_config(params: &SaveConfigParams) -> Result<()> {
    if params.provider.trim().is_empty() {
        return Err(anyhow!("Provider 不能为空"));
    }
    if params.api_key.trim().is_empty() {
        return Err(anyhow!("API Key 不能为空"));
    }
    if params.model_id.trim().is_empty() {
        return Err(anyhow!("Model ID 不能为空"));
    }

    let mut config = read_user_config();
    ensure_object(&mut config);
    ensure_object_path(&mut config, &["models", "providers"]);
    ensure_object_path(&mut config, &["agents", "defaults", "model"]);
    ensure_object_path(&mut config, &["agents", "defaults", "compaction"]);

    config["agents"]["defaults"]["compaction"]["mode"] = Value::String("safeguard".to_string());

    if params.provider == "moonshot" {
        let (provider_key, provider_config) = provider::build_moonshot_provider_config(
            &params.api_key,
            &params.model_id,
            params.sub_platform.as_deref(),
        );
        config["models"]["providers"][provider_key.as_str()] = provider_config;
        config["agents"]["defaults"]["model"]["primary"] =
            Value::String(format!("{provider_key}/{}", params.model_id));
    } else {
        let provider_like = SaveConfigParamsLike {
            provider: params.provider.clone(),
            api_key: params.api_key.clone(),
            model_id: params.model_id.clone(),
            base_url: params.base_url.clone(),
            api: params.api.clone(),
            sub_platform: params.sub_platform.clone(),
            support_image: params.support_image,
            custom_preset: params.custom_preset.clone(),
        };
        let config_key = provider::provider_config_key(
            &params.provider,
            params.base_url.as_deref(),
            params.custom_preset.as_deref(),
        );
        config["models"]["providers"][config_key.as_str()] = provider::build_provider_config(&provider_like);
        config["agents"]["defaults"]["model"]["primary"] =
            Value::String(format!("{config_key}/{}", params.model_id));
    }

    ensure_gateway_auth_token_in_config(&mut config);

    ensure_object_path(&mut config, &["browser"]);
    config["browser"]["defaultProfile"] = Value::String("openclaw".to_string());

    ensure_object_path(&mut config, &["channels", "imessage"]);
    config["channels"]["imessage"]["enabled"] = Value::Bool(false);

    ensure_object_path(&mut config, &["update"]);
    config["update"]["checkOnStart"] = Value::Bool(false);

    ensure_object_path(&mut config, &["tools"]);
    config["tools"]["profile"] = Value::String("full".to_string());

    if let Some(object) = config.as_object_mut() {
        object.remove("wizard");
    }

    write_user_config(&config)
}

pub fn complete_setup(params: Option<&CompleteSetupParams>) -> Result<String> {
    let mut config = read_user_config();
    ensure_object(&mut config);

    let session_memory = params.and_then(|value| value.session_memory).unwrap_or(true);
    ensure_object_path(&mut config, &["hooks", "internal", "entries"]);
    config["hooks"]["internal"]["enabled"] = Value::Bool(true);
    config["hooks"]["internal"]["entries"]["session-memory"] = json!({ "enabled": session_memory });

    ensure_object_path(&mut config, &["wizard"]);
    config["wizard"]["lastRunAt"] = Value::String(Utc::now().to_rfc3339());
    if let Some(wizard) = config["wizard"].as_object_mut() {
        wizard.remove("pendingAt");
    }

    let token = ensure_gateway_auth_token_in_config(&mut config);
    write_user_config(&config)?;
    mark_setup_complete()?;
    record_setup_baseline_config_snapshot()?;

    if let Some(launch_at_login) = params.and_then(|value| value.launch_at_login) {
        system::set_launch_at_login_enabled(launch_at_login)?;
    }

    if params.and_then(|value| value.install_cli).unwrap_or(true) {
        system::install_cli_best_effort()?;
    } else {
        system::uninstall_cli_best_effort()?;
    }

    Ok(token)
}

pub fn ensure_gateway_auth_token_in_config(config: &mut Value) -> String {
    ensure_object_path(config, &["gateway", "auth"]);
    let existing = config["gateway"]["auth"]["token"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let token = existing.unwrap_or_else(random_token);

    config["gateway"]["auth"]["mode"] = Value::String("token".to_string());
    config["gateway"]["auth"]["token"] = Value::String(token.clone());
    if config["gateway"]["mode"].as_str().unwrap_or_default().trim().is_empty() {
        config["gateway"]["mode"] = Value::String("local".to_string());
    }

    ensure_object_path(config, &["gateway", "controlUi"]);
    let origins = config["gateway"]["controlUi"]["allowedOrigins"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut normalized: Vec<Value> = origins
        .into_iter()
        .filter_map(|value| value.as_str().map(|s| s.trim().to_string()))
        .filter(|value| !value.is_empty())
        .map(Value::String)
        .collect();
    let has_null_origin = normalized
        .iter()
        .any(|value| value.as_str().is_some_and(|s| s.eq_ignore_ascii_case("null")));
    if !has_null_origin {
        normalized.push(Value::String("null".to_string()));
    }
    config["gateway"]["controlUi"]["allowedOrigins"] = Value::Array(normalized);

    token
}

pub fn write_user_config(config: &Value) -> Result<()> {
    let path = user_config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("创建状态目录失败: {}", parent.display()))?;
    }
    backup_current_user_config()?;
    let raw = serde_json::to_string_pretty(config)? + "\n";
    fs::write(&path, raw).with_context(|| format!("写入配置失败: {}", path.display()))?;
    sync_openclaw_state_after_write(&path)?;
    Ok(())
}

pub fn mark_setup_complete() -> Result<()> {
    let path = oneclaw_config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut config = read_json_file(&path).unwrap_or_else(|| json!({}));
    ensure_object(&mut config);
    config["setupCompletedAt"] = Value::String(Utc::now().to_rfc3339());
    fs::write(&path, serde_json::to_string_pretty(&config)? + "\n")?;
    Ok(())
}

pub fn record_setup_baseline_config_snapshot() -> Result<()> {
    let config_path = user_config_path();
    let raw = read_valid_json_raw(&config_path);
    let Some(raw) = raw else {
        return Ok(());
    };
    let baseline_path = state_dir().join("openclaw-setup-baseline.json");
    if baseline_path.exists() {
        return Ok(());
    }
    if let Some(parent) = baseline_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(baseline_path, raw)?;
    Ok(())
}

pub fn sync_openclaw_state_after_write(config_path: &Path) -> Result<()> {
    let backup_path = PathBuf::from(format!("{}.bak", config_path.display()));
    let _ = fs::copy(config_path, backup_path);
    reset_config_health_baseline(config_path)?;
    Ok(())
}

pub fn reset_config_health_baseline(config_path: &Path) -> Result<()> {
    let health_path = state_dir().join("logs").join("config-health.json");
    if !health_path.exists() {
        return Ok(());
    }
    let Some(mut health) = read_json_file(&health_path) else {
        return Ok(());
    };
    let target = normalize_path_for_compare(config_path);
    let Some(entries) = health.get_mut("entries").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    let keys: Vec<String> = entries
        .keys()
        .filter(|key| normalize_path_for_compare(Path::new(key)) == target)
        .cloned()
        .collect();
    if keys.is_empty() {
        return Ok(());
    }
    for key in keys {
        entries.remove(&key);
    }
    fs::write(health_path, serde_json::to_string_pretty(&health)? + "\n")?;
    Ok(())
}

fn backup_current_user_config() -> Result<()> {
    let path = user_config_path();
    let Some(raw) = read_valid_json_raw(&path) else {
        return Ok(());
    };
    let backup_dir = state_dir().join("config-backups");
    fs::create_dir_all(&backup_dir)?;
    let file_name = format!("openclaw-{}.json", Utc::now().format("%Y%m%d-%H%M%S"));
    fs::write(backup_dir.join(file_name), raw)?;
    Ok(())
}

fn read_json_file(path: &Path) -> Option<Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .filter(Value::is_object)
}

fn read_valid_json_raw(path: &Path) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str::<Value>(&raw).ok()?;
    Some(raw)
}

fn normalize_path_for_compare(path: &Path) -> String {
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let text = resolved.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        text.to_ascii_lowercase()
    } else {
        text
    }
}

fn ensure_object(value: &mut Value) {
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
}

fn ensure_object_path<'a>(root: &'a mut Value, path: &[&str]) -> &'a mut Map<String, Value> {
    ensure_object(root);
    let mut current = root;
    for key in path {
        let object = current.as_object_mut().expect("object ensured");
        current = object
            .entry((*key).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        ensure_object(current);
    }
    current.as_object_mut().expect("object ensured")
}

fn random_token() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
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

    fn temp_state() -> (TempDir, EnvGuard) {
        let dir = TempDir::new().unwrap();
        let guard = EnvGuard::set("OPENCLAW_STATE_DIR", dir.path());
        (dir, guard)
    }

    #[test]
    #[serial(openclaw_env)]
    fn save_setup_config_writes_provider_defaults_and_gateway_token() {
        let (_dir, _guard) = temp_state();
        save_setup_config(&SaveConfigParams {
            provider: "openai".to_string(),
            api_key: "sk-test".to_string(),
            model_id: "gpt-test".to_string(),
            base_url: None,
            api: None,
            sub_platform: None,
            support_image: Some(true),
            custom_preset: None,
        })
        .unwrap();

        let config = read_user_config();
        assert_eq!(config["models"]["providers"]["openai"]["baseUrl"], "https://api.openai.com/v1");
        assert_eq!(config["agents"]["defaults"]["model"]["primary"], "openai/gpt-test");
        assert_eq!(config["gateway"]["auth"]["mode"], "token");
        assert_eq!(config["channels"]["imessage"]["enabled"], false);
        assert!(user_config_path().with_extension("json.bak").exists() || PathBuf::from(format!("{}.bak", user_config_path().display())).exists());
    }

    #[test]
    #[serial(openclaw_env)]
    fn save_setup_config_derives_custom_provider_key() {
        let (_dir, _guard) = temp_state();
        save_setup_config(&SaveConfigParams {
            provider: "custom".to_string(),
            api_key: "sk-test".to_string(),
            model_id: "model-x".to_string(),
            base_url: Some("https://api.example.com/v1".to_string()),
            api: Some("openai-completions".to_string()),
            sub_platform: None,
            support_image: Some(false),
            custom_preset: None,
        })
        .unwrap();

        let config = read_user_config();
        assert_eq!(
            config["agents"]["defaults"]["model"]["primary"],
            "custom-api-example-com-v1/model-x"
        );
        assert_eq!(
            config["models"]["providers"]["custom-api-example-com-v1"]["models"][0]["input"],
            json!(["text"])
        );
    }

    #[test]
    #[serial(openclaw_env)]
    fn complete_setup_marks_oneclaw_and_records_baseline() {
        let (_dir, _guard) = temp_state();
        save_setup_config(&SaveConfigParams {
            provider: "anthropic".to_string(),
            api_key: "sk-ant-test".to_string(),
            model_id: "claude-test".to_string(),
            base_url: None,
            api: None,
            sub_platform: None,
            support_image: Some(true),
            custom_preset: None,
        })
        .unwrap();

        let token = complete_setup(Some(&CompleteSetupParams {
            install_cli: Some(false),
            launch_at_login: None,
            session_memory: Some(true),
        }))
        .unwrap();

        assert!(!token.is_empty());
        let config = read_user_config();
        assert!(config["wizard"]["lastRunAt"].as_str().is_some());
        assert_eq!(config["hooks"]["internal"]["entries"]["session-memory"]["enabled"], true);
        let oneclaw = read_json_file(&oneclaw_config_path()).unwrap();
        assert!(oneclaw["setupCompletedAt"].as_str().is_some());
        assert!(state_dir().join("openclaw-setup-baseline.json").exists());
    }

    #[test]
    #[serial(openclaw_env)]
    fn reset_config_health_baseline_removes_matching_entry_only() {
        let (_dir, _guard) = temp_state();
        let config_path = user_config_path();
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(&config_path, "{}").unwrap();
        let logs = state_dir().join("logs");
        fs::create_dir_all(&logs).unwrap();
        let other = state_dir().join("other.json");
        fs::write(
            logs.join("config-health.json"),
            json!({
                "entries": {
                    config_path.to_string_lossy().to_string(): {"hash": "old"},
                    other.to_string_lossy().to_string(): {"hash": "keep"}
                }
            })
            .to_string(),
        )
        .unwrap();

        reset_config_health_baseline(&config_path).unwrap();
        let health = read_json_file(&logs.join("config-health.json")).unwrap();
        assert!(health["entries"].get(config_path.to_string_lossy().as_ref()).is_none());
        assert!(health["entries"].get(other.to_string_lossy().as_ref()).is_some());
    }
}
