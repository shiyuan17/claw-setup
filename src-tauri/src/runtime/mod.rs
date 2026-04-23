use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLayout {
    pub target_id: String,
    pub resources_dir: PathBuf,
    pub node_bin: PathBuf,
    pub npm_bin: PathBuf,
    pub gateway_entry: PathBuf,
    pub gateway_cwd: PathBuf,
    pub clawhub_entry: PathBuf,
}

pub fn resolve_runtime_layout() -> Result<RuntimeLayout> {
    let target_id = current_target_id()?;
    let resources_dir = resolve_resources_dir(&target_id);
    let node_bin = resources_dir
        .join("runtime")
        .join(if cfg!(windows) { "node.exe" } else { "node" });
    let npm_bin = resources_dir
        .join("runtime")
        .join(if cfg!(windows) { "npm.cmd" } else { "npm" });
    let gateway_root = if resources_dir.join("gateway.asar").exists() {
        resources_dir.join("gateway.asar")
    } else {
        resources_dir.join("gateway")
    };
    let gateway_entry = {
        let openclaw_mjs = gateway_root
            .join("node_modules")
            .join("openclaw")
            .join("openclaw.mjs");
        if openclaw_mjs.exists() {
            openclaw_mjs
        } else {
            gateway_root.join("gateway-entry.mjs")
        }
    };
    let gateway_cwd = if gateway_root.extension().and_then(|value| value.to_str()) == Some("asar") {
        crate::config::state_dir()
    } else {
        gateway_root.join("node_modules").join("openclaw")
    };
    let clawhub_entry = gateway_root
        .join("node_modules")
        .join("clawhub")
        .join("bin")
        .join("clawdhub.js");

    Ok(RuntimeLayout {
        target_id,
        resources_dir,
        node_bin,
        npm_bin,
        gateway_entry,
        gateway_cwd,
        clawhub_entry,
    })
}

pub fn validate_runtime_layout(layout: &RuntimeLayout) -> Result<()> {
    ensure_exists(&layout.node_bin, "Node.js runtime 不存在")?;
    ensure_exists(&layout.gateway_entry, "openclaw gateway 入口不存在")?;
    ensure_exists(&layout.gateway_cwd, "openclaw gateway cwd 不存在")?;
    Ok(())
}

pub fn current_target_id() -> Result<String> {
    if let Ok(value) = std::env::var("ONECLAW_TARGET") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    map_target_id(std::env::consts::OS, std::env::consts::ARCH)
        .map(str::to_string)
}

pub fn map_target_id(os: &str, arch: &str) -> Result<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Ok("darwin-arm64"),
        ("macos", "x86_64") => Ok("darwin-x64"),
        ("windows", "x86_64") => Ok("win32-x64"),
        ("windows", "aarch64") => Ok("win32-arm64"),
        _ => Err(anyhow!("不支持的运行平台: {os}-{arch}")),
    }
}

fn resolve_resources_dir(target_id: &str) -> PathBuf {
    if let Some(path) = resolve_resources_dir_from_env(target_id) {
        return path;
    }

    if let Some(path) = resolve_packaged_resources_dir(target_id) {
        return path;
    }

    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("resources")
        .join("targets")
        .join(target_id)
}

fn resolve_resources_dir_from_env(target_id: &str) -> Option<PathBuf> {
    let value = std::env::var("ONECLAW_RESOURCES_DIR").ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let candidate = PathBuf::from(trimmed);
    if candidate.join("runtime").exists() {
        return Some(candidate);
    }
    for nested in [
        candidate.join("resources").join("targets").join(target_id),
        candidate.join("targets").join(target_id),
        candidate.join(target_id),
    ] {
        if nested.join("runtime").exists() {
            return Some(nested);
        }
    }
    Some(candidate)
}

fn resolve_packaged_resources_dir(target_id: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;

    let packaged_root = if cfg!(target_os = "macos") {
        exe_dir.parent()?.join("Resources")
    } else if cfg!(target_os = "windows") {
        exe_dir.to_path_buf()
    } else {
        return None;
    };

    [
        packaged_root.join("resources").join("targets").join(target_id),
        packaged_root.join("targets").join(target_id),
        packaged_root.join(target_id),
        packaged_root.clone(),
    ]
    .into_iter()
    .find(|nested| nested.join("runtime").exists())
}

fn ensure_exists(path: &Path, message: &str) -> Result<()> {
    if path.exists() {
        Ok(())
    } else {
        Err(anyhow!("{message}: {}", path.display()))
    }
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

        fn set_value(key: &'static str, value: &str) -> Self {
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
    fn maps_rust_platforms_to_oneclaw_targets() {
        assert_eq!(map_target_id("macos", "aarch64").unwrap(), "darwin-arm64");
        assert_eq!(map_target_id("macos", "x86_64").unwrap(), "darwin-x64");
        assert_eq!(map_target_id("windows", "x86_64").unwrap(), "win32-x64");
        assert_eq!(map_target_id("windows", "aarch64").unwrap(), "win32-arm64");
    }

    #[test]
    #[serial(openclaw_env)]
    fn resolves_layout_from_env_resources_dir() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("runtime")).unwrap();
        let _guard = EnvGuard::set("ONECLAW_RESOURCES_DIR", dir.path());
        let layout = resolve_runtime_layout().unwrap();
        assert_eq!(layout.resources_dir, dir.path());
        assert!(layout.node_bin.ends_with(if cfg!(windows) { "node.exe" } else { "node" }));
    }

    #[test]
    #[serial(openclaw_env)]
    fn resolves_layout_from_nested_packaged_style_env_root() {
        let dir = TempDir::new().unwrap();
        let _target = EnvGuard::set_value("ONECLAW_TARGET", "darwin-arm64");
        let nested = dir.path().join("resources").join("targets").join("darwin-arm64").join("runtime");
        std::fs::create_dir_all(&nested).unwrap();
        let _guard = EnvGuard::set("ONECLAW_RESOURCES_DIR", dir.path());
        let layout = resolve_runtime_layout().unwrap();
        assert!(layout.resources_dir.ends_with("darwin-arm64"));
    }
}
