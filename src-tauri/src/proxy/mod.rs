use std::net::TcpListener;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use reqwest::blocking::Client;
use tiny_http::{Header, Method, Response, Server, StatusCode};

const UPSTREAM_BASE: &str = "https://api.kimi.com";
const DEFAULT_PROXY_PORT: u16 = 18_790;

#[derive(Debug, Clone, Default)]
pub struct ProxyState {
    access_token: String,
    search_key: String,
    port: Option<u16>,
}

static STATE: OnceLock<Arc<Mutex<ProxyState>>> = OnceLock::new();

fn state() -> Arc<Mutex<ProxyState>> {
    STATE
        .get_or_init(|| Arc::new(Mutex::new(ProxyState::default())))
        .clone()
}

pub fn set_access_token(token: impl Into<String>) {
    if let Ok(mut state) = state().lock() {
        state.access_token = token.into();
    }
}

pub fn set_search_key(token: impl Into<String>) {
    if let Ok(mut state) = state().lock() {
        state.search_key = token.into();
    }
}

pub fn get_port() -> Option<u16> {
    state().lock().ok().and_then(|state| state.port)
}

pub fn start_auth_proxy(
    preferred_port: Option<u16>,
    exclude_port: Option<u16>,
    access_token: impl Into<String>,
    search_key: Option<String>,
) -> Result<u16> {
    set_access_token(access_token);
    if let Some(search_key) = search_key {
        set_search_key(search_key);
    }

    if let Some(port) = get_port() {
        return Ok(port);
    }

    let (server, port) = bind_server(preferred_port, exclude_port)?;
    if let Ok(mut state) = state().lock() {
        state.port = Some(port);
    }
    let shared = state();
    thread::spawn(move || {
        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .ok();
        for request in server.incoming_requests() {
            let Some(client) = &client else {
                let _ = request.respond(text_response(500, "HTTP client unavailable"));
                continue;
            };
            let response = handle_proxy_request(client, shared.clone(), request);
            if let Err(err) = response {
                eprintln!("[claw-setup proxy] {err:#}");
            }
        }
    });

    Ok(port)
}

fn bind_server(preferred_port: Option<u16>, exclude_port: Option<u16>) -> Result<(Server, u16)> {
    let mut candidates = Vec::new();
    if let Some(port) = preferred_port.filter(|port| Some(*port) != exclude_port) {
        candidates.push(port);
    }
    if Some(DEFAULT_PROXY_PORT) != exclude_port && !candidates.contains(&DEFAULT_PROXY_PORT) {
        candidates.push(DEFAULT_PROXY_PORT);
    }
    candidates.push(0);

    for port in candidates {
        if port != 0 && TcpListener::bind(("127.0.0.1", port)).is_err() {
            continue;
        }
        let server = Server::http(("127.0.0.1", port)).map_err(|err| anyhow!("{err}"))?;
        let actual = server
            .server_addr()
            .to_ip()
            .map(|addr| addr.port())
            .ok_or_else(|| anyhow!("无法解析 auth proxy 监听端口"))?;
        return Ok((server, actual));
    }
    Err(anyhow!("无法启动 Kimi auth proxy"))
}

fn handle_proxy_request(
    client: &Client,
    shared: Arc<Mutex<ProxyState>>,
    mut request: tiny_http::Request,
) -> Result<()> {
    let url = request.url().to_string();
    let path_only = url.split('?').next().unwrap_or("/");
    if !(path_only == "/coding" || path_only.starts_with("/coding/")) {
        request.respond(text_response(404, "Not Found")).ok();
        return Ok(());
    }

    let use_search_key = path_only.contains("/v1/search") || path_only.contains("/v1/fetch");
    let token = {
        let state = shared.lock().map_err(|_| anyhow!("proxy state poisoned"))?;
        if use_search_key && !state.search_key.trim().is_empty() {
            state.search_key.clone()
        } else {
            state.access_token.clone()
        }
    };
    if token.trim().is_empty() {
        request.respond(text_response(401, "No access token available")).ok();
        return Ok(());
    }

    let mut body = Vec::new();
    request.as_reader().read_to_end(&mut body)?;
    let method = to_reqwest_method(request.method())?;
    let mut upstream = client.request(method, format!("{UPSTREAM_BASE}{url}"));
    for header in request.headers() {
        let key = header.field.as_str().to_string().to_ascii_lowercase();
        if key == "host" || key == "connection" || key == "content-length" {
            continue;
        }
        upstream = upstream.header(key.as_str(), header.value.as_str());
    }
    upstream = upstream
        .header("host", "api.kimi.com")
        .header("x-api-key", token.clone())
        .header("authorization", format!("Bearer {token}"))
        .body(body);

    let upstream_response = upstream.send().context("Kimi 上游请求失败")?;
    let status = upstream_response.status().as_u16();
    let content_type = upstream_response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let bytes = upstream_response.bytes().context("读取 Kimi 上游响应失败")?;
    let response = Response::from_data(bytes.to_vec())
        .with_status_code(StatusCode(status))
        .with_header(Header::from_bytes("content-type", content_type).map_err(|_| anyhow!("无效响应头"))?);
    request.respond(response).ok();
    Ok(())
}

fn to_reqwest_method(method: &Method) -> Result<reqwest::Method> {
    reqwest::Method::from_bytes(method.as_str().as_bytes()).context("无效代理请求方法")
}

fn text_response(status: u16, text: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(text.to_string()).with_status_code(StatusCode(status))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_proxy_access_token_without_exposing_it_publicly() {
        set_access_token("secret");
        assert!(state().lock().unwrap().access_token == "secret");
    }

    #[test]
    fn binds_proxy_to_dynamic_port() {
        let port = start_auth_proxy(Some(0), None, "token", None).unwrap();
        assert!(port > 0);
        assert_eq!(get_port(), Some(port));
    }
}
