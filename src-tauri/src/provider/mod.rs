use std::collections::BTreeMap;

use anyhow::{anyhow, Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyKeyParams {
    pub provider: String,
    pub api_key: Option<String>,
    #[serde(rename = "baseURL", alias = "baseUrl")]
    pub base_url: Option<String>,
    pub sub_platform: Option<String>,
    pub api_type: Option<String>,
    #[serde(rename = "modelID", alias = "modelId")]
    pub model_id: Option<String>,
    pub custom_preset: Option<String>,
    pub proxy_port: Option<u16>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveConfigParamsLike {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderPreset {
    pub base_url: &'static str,
    pub api: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomProviderPreset {
    pub provider_key: &'static str,
    pub base_url: &'static str,
    pub api: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoonshotSubPlatform {
    pub base_url: &'static str,
    pub api: &'static str,
    pub provider_key: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationRequest {
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

const UA_ANTHROPIC: &str = "Anthropic/JS 0.73.0";
const UA_OPENAI: &str = "OpenAI/JS 6.10.0";

pub fn provider_preset(provider: &str) -> Option<ProviderPreset> {
    match provider {
        "anthropic" => Some(ProviderPreset {
            base_url: "https://api.anthropic.com/v1",
            api: "anthropic-messages",
        }),
        "openai" => Some(ProviderPreset {
            base_url: "https://api.openai.com/v1",
            api: "openai-completions",
        }),
        "google" => Some(ProviderPreset {
            base_url: "https://generativelanguage.googleapis.com/v1beta",
            api: "google-generative-ai",
        }),
        _ => None,
    }
}

pub fn moonshot_sub_platform(sub_platform: Option<&str>) -> MoonshotSubPlatform {
    match sub_platform.unwrap_or("moonshot-cn") {
        "moonshot-ai" => MoonshotSubPlatform {
            base_url: "https://api.moonshot.ai/v1",
            api: "openai-completions",
            provider_key: "moonshot",
        },
        "kimi-code" => MoonshotSubPlatform {
            base_url: "https://api.kimi.com/coding",
            api: "anthropic-messages",
            provider_key: "kimi-coding",
        },
        _ => MoonshotSubPlatform {
            base_url: "https://api.moonshot.cn/v1",
            api: "openai-completions",
            provider_key: "moonshot",
        },
    }
}

pub fn custom_provider_preset(custom_preset: Option<&str>) -> Option<CustomProviderPreset> {
    match custom_preset.filter(|value| !value.trim().is_empty())? {
        "minimax" => Some(CustomProviderPreset {
            provider_key: "minimax",
            base_url: "https://api.minimax.io/anthropic",
            api: "anthropic-messages",
        }),
        "minimax-cn" => Some(CustomProviderPreset {
            provider_key: "minimax-cn",
            base_url: "https://api.minimaxi.com/anthropic",
            api: "anthropic-messages",
        }),
        "zai-global" => Some(CustomProviderPreset {
            provider_key: "zai-global",
            base_url: "https://api.z.ai/api/paas/v4",
            api: "openai-completions",
        }),
        "zai-cn" => Some(CustomProviderPreset {
            provider_key: "zai-cn",
            base_url: "https://open.bigmodel.cn/api/paas/v4",
            api: "openai-completions",
        }),
        "zai-cn-coding" => Some(CustomProviderPreset {
            provider_key: "zai-cn-coding",
            base_url: "https://open.bigmodel.cn/api/coding/paas/v4",
            api: "openai-completions",
        }),
        "volcengine" => Some(CustomProviderPreset {
            provider_key: "volcengine",
            base_url: "https://ark.cn-beijing.volces.com/api/v3",
            api: "openai-completions",
        }),
        "volcengine-coding" => Some(CustomProviderPreset {
            provider_key: "volcengine-coding",
            base_url: "https://ark.cn-beijing.volces.com/api/coding",
            api: "anthropic-messages",
        }),
        "qwen" => Some(CustomProviderPreset {
            provider_key: "qwen",
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
            api: "openai-completions",
        }),
        "qwen-coding" => Some(CustomProviderPreset {
            provider_key: "qwen-coding",
            base_url: "https://coding.dashscope.aliyuncs.com/v1",
            api: "openai-completions",
        }),
        "deepseek" => Some(CustomProviderPreset {
            provider_key: "deepseek",
            base_url: "https://api.deepseek.com",
            api: "openai-completions",
        }),
        _ => None,
    }
}

pub fn derive_custom_config_key(base_url: &str) -> String {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return "custom".to_string();
    };
    let mut raw = format!("{}{}", url.host_str().unwrap_or_default(), url.path());
    while raw.ends_with('/') {
        raw.pop();
    }
    let mut slug = String::new();
    let mut last_was_dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "custom".to_string()
    } else {
        format!("custom-{slug}")
    }
}

pub fn provider_config_key(provider: &str, base_url: Option<&str>, custom_preset: Option<&str>) -> String {
    if let Some(custom) = custom_provider_preset(custom_preset) {
        return custom.provider_key.to_string();
    }
    if provider == "custom" {
        return base_url
            .filter(|value| !value.trim().is_empty())
            .map(derive_custom_config_key)
            .unwrap_or_else(|| "custom".to_string());
    }
    provider.to_string()
}

pub fn build_provider_config(params: &SaveConfigParamsLike) -> Value {
    if let Some(preset) = provider_preset(&params.provider) {
        return provider_entry(
            &params.api_key,
            preset.base_url,
            preset.api,
            &params.model_id,
            true,
            false,
        );
    }

    if let Some(custom) = custom_provider_preset(params.custom_preset.as_deref()) {
        return provider_entry(
            &params.api_key,
            params.base_url.as_deref().filter(|value| !value.trim().is_empty()).unwrap_or(custom.base_url),
            custom.api,
            &params.model_id,
            true,
            false,
        );
    }

    let input_image = params.support_image.unwrap_or(true);
    provider_entry(
        &params.api_key,
        params.base_url.as_deref().unwrap_or_default(),
        params.api.as_deref().unwrap_or("openai-completions"),
        &params.model_id,
        input_image,
        false,
    )
}

pub fn build_moonshot_provider_config(api_key: &str, model_id: &str, sub_platform: Option<&str>) -> (String, Value) {
    let sub = moonshot_sub_platform(sub_platform);
    (
        sub.provider_key.to_string(),
        provider_entry(api_key, sub.base_url, sub.api, model_id, true, true),
    )
}

fn provider_entry(api_key: &str, base_url: &str, api: &str, model_id: &str, input_image: bool, reasoning: bool) -> Value {
    let mut model = json!({
        "id": model_id,
        "name": model_id,
        "input": if input_image { json!(["text", "image"]) } else { json!(["text"]) },
    });
    if reasoning {
        model["reasoning"] = Value::Bool(true);
    }

    json!({
        "apiKey": api_key,
        "baseUrl": base_url,
        "api": api,
        "models": [model],
    })
}

pub fn build_verification_request(params: &VerifyKeyParams) -> Result<VerificationRequest> {
    let provider = params.provider.as_str();
    let api_key = required(params.api_key.as_deref(), "API Key 不能为空")?;
    match provider {
        "anthropic" => Ok(VerificationRequest {
            method: "POST".to_string(),
            url: "https://api.anthropic.com/v1/messages".to_string(),
            headers: headers([
                ("x-api-key", api_key),
                ("anthropic-version", "2023-06-01"),
                ("content-type", "application/json"),
            ]),
            body: Some(json!({
                "model": params.model_id.as_deref().unwrap_or("claude-haiku-4-5-20251001"),
                "max_tokens": 1,
                "messages": [{"role": "user", "content": "hi"}],
            })),
        }),
        "openai" => Ok(VerificationRequest {
            method: "GET".to_string(),
            url: "https://api.openai.com/v1/models".to_string(),
            headers: headers([("authorization", format!("Bearer {api_key}"))]),
            body: None,
        }),
        "google" => Ok(VerificationRequest {
            method: "GET".to_string(),
            url: format!("https://generativelanguage.googleapis.com/v1beta/models?key={api_key}"),
            headers: BTreeMap::new(),
            body: None,
        }),
        "moonshot" => {
            let sub = moonshot_sub_platform(params.sub_platform.as_deref());
            if params.sub_platform.as_deref() == Some("kimi-code") {
                Ok(VerificationRequest {
                    method: "POST".to_string(),
                    url: format!("{}/v1/messages", sub.base_url),
                    headers: headers([
                        ("authorization", format!("Bearer {api_key}")),
                        ("anthropic-version", "2023-06-01".to_string()),
                        ("content-type", "application/json".to_string()),
                    ]),
                    body: Some(json!({
                        "model": params.model_id.as_deref().unwrap_or("k2p5"),
                        "max_tokens": 1,
                        "messages": [{"role": "user", "content": "hi"}],
                    })),
                })
            } else {
                Ok(VerificationRequest {
                    method: "GET".to_string(),
                    url: format!("{}/models", sub.base_url),
                    headers: headers([("authorization", format!("Bearer {api_key}"))]),
                    body: None,
                })
            }
        }
        "custom" => build_custom_verification_request(params, api_key),
        _ => Err(anyhow!("未知 Provider: {provider}")),
    }
}

pub fn verify_provider(params: &VerifyKeyParams) -> Result<()> {
    let request = build_verification_request(params)?;
    execute_verification_request(&request)
}

pub fn execute_verification_request(request: &VerificationRequest) -> Result<()> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("创建 HTTP client 失败")?;
    let method = reqwest::Method::from_bytes(request.method.as_bytes()).context("无效 HTTP method")?;
    let mut builder = client.request(method, &request.url);
    for (key, value) in &request.headers {
        builder = builder.header(key, value);
    }
    if let Some(body) = &request.body {
        builder = builder.json(body);
    }
    let response = builder.send().context("网络错误")?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response.text().unwrap_or_default();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        Err(anyhow!("API Key 无效 ({})", status.as_u16()))
    } else {
        Err(anyhow!("请求失败 ({}): {}", status.as_u16(), body.chars().take(200).collect::<String>()))
    }
}

pub fn public_error(err: &anyhow::Error) -> String {
    redact_secret(&err.to_string())
}

pub fn redact_secret(message: &str) -> String {
    let mut redacted = Vec::new();
    for token in message.split_whitespace() {
        if token.starts_with("sk-") || token.starts_with("sk_ant_") || token.len() >= 48 {
            redacted.push("***");
        } else {
            redacted.push(token);
        }
    }
    redacted.join(" ")
}

fn build_custom_verification_request(params: &VerifyKeyParams, api_key: &str) -> Result<VerificationRequest> {
    let custom = custom_provider_preset(params.custom_preset.as_deref());
    let base = params
        .base_url
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .or(custom.as_ref().map(|preset| preset.base_url))
        .ok_or_else(|| anyhow!("Custom provider 需要 Base URL"))?
        .trim_end_matches('/');
    let api_type = custom
        .as_ref()
        .map(|preset| preset.api)
        .or(params.api_type.as_deref())
        .unwrap_or("openai-completions");
    let model_id = required(params.model_id.as_deref(), "Custom provider 需要 Model ID")?;

    if api_type == "anthropic-messages" {
        Ok(VerificationRequest {
            method: "POST".to_string(),
            url: format!("{base}/v1/messages"),
            headers: headers([
                ("user-agent", UA_ANTHROPIC.to_string()),
                ("x-api-key", api_key.to_string()),
                ("anthropic-version", "2023-06-01".to_string()),
                ("content-type", "application/json".to_string()),
            ]),
            body: Some(json!({
                "model": model_id,
                "max_tokens": 1,
                "messages": [{"role": "user", "content": "hi"}],
            })),
        })
    } else if api_type == "openai-responses" {
        Ok(VerificationRequest {
            method: "POST".to_string(),
            url: format!("{base}/v1/responses"),
            headers: headers([
                ("user-agent", UA_OPENAI.to_string()),
                ("authorization", format!("Bearer {api_key}")),
                ("content-type", "application/json".to_string()),
            ]),
            body: Some(json!({
                "model": model_id,
                "input": "hi",
            })),
        })
    } else {
        Ok(VerificationRequest {
            method: "POST".to_string(),
            url: format!("{base}/chat/completions"),
            headers: headers([
                ("user-agent", UA_OPENAI.to_string()),
                ("authorization", format!("Bearer {api_key}")),
                ("content-type", "application/json".to_string()),
            ]),
            body: Some(json!({
                "model": model_id,
                "max_tokens": 1,
                "messages": [{"role": "user", "content": "hi"}],
            })),
        })
    }
}

fn required<'a>(value: Option<&'a str>, message: &str) -> Result<&'a str> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!(message.to_string()))
}

fn headers<const N: usize, K, V>(entries: [(K, V); N]) -> BTreeMap<String, String>
where
    K: Into<String>,
    V: Into<String>,
{
    entries
        .into_iter()
        .map(|(key, value)| (key.into().to_ascii_lowercase(), value.into()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(provider: &str) -> VerifyKeyParams {
        VerifyKeyParams {
            provider: provider.to_string(),
            api_key: Some("sk-test".to_string()),
            base_url: None,
            sub_platform: None,
            api_type: None,
            model_id: None,
            custom_preset: None,
            proxy_port: None,
        }
    }

    #[test]
    fn derives_stable_custom_config_key_from_url() {
        assert_eq!(
            derive_custom_config_key("https://api.example.com/v1/"),
            "custom-api-example-com-v1"
        );
    }

    #[test]
    fn maps_custom_preset_to_provider_key() {
        assert_eq!(
            provider_config_key("custom", Some("https://ignored.example"), Some("qwen-coding")),
            "qwen-coding"
        );
    }

    #[test]
    fn builds_openai_models_verification_request() {
        let request = build_verification_request(&params("openai")).unwrap();
        assert_eq!(request.method, "GET");
        assert_eq!(request.url, "https://api.openai.com/v1/models");
        assert_eq!(request.headers["authorization"], "Bearer sk-test");
        assert!(request.body.is_none());
    }

    #[test]
    fn builds_anthropic_message_verification_request() {
        let mut params = params("anthropic");
        params.model_id = Some("claude-test".to_string());
        let request = build_verification_request(&params).unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "https://api.anthropic.com/v1/messages");
        assert_eq!(request.headers["x-api-key"], "sk-test");
        assert_eq!(request.body.unwrap()["model"], "claude-test");
    }

    #[test]
    fn builds_kimi_code_direct_verification_request() {
        let mut params = params("moonshot");
        params.sub_platform = Some("kimi-code".to_string());
        params.model_id = Some("k2p5".to_string());
        let request = build_verification_request(&params).unwrap();
        assert_eq!(request.url, "https://api.kimi.com/coding/v1/messages");
        assert_eq!(request.headers["authorization"], "Bearer sk-test");
    }

    #[test]
    fn builds_custom_anthropic_verification_request() {
        let mut params = params("custom");
        params.base_url = Some("https://api.example.com".to_string());
        params.api_type = Some("anthropic-messages".to_string());
        params.model_id = Some("model-x".to_string());
        let request = build_verification_request(&params).unwrap();
        assert_eq!(request.url, "https://api.example.com/v1/messages");
        assert_eq!(request.headers["user-agent"], UA_ANTHROPIC);
    }

    #[test]
    fn redacts_obvious_secrets_from_errors() {
        assert_eq!(
            redact_secret("failed for sk-abcdefg and longtokenlongtokenlongtokenlongtokenlongtokenlongtokenlongtoken"),
            "failed for *** and ***"
        );
    }
}
