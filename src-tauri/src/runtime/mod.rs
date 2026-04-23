use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLayout {
    pub resources_dir: PathBuf,
    pub node_bin: PathBuf,
    pub npm_bin: PathBuf,
    pub gateway_entry: PathBuf,
    pub gateway_cwd: PathBuf,
}

pub fn resolve_runtime_layout() -> Result<RuntimeLayout> {
    let resources_dir = resolve_resources_dir();
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

    Ok(RuntimeLayout {
        resources_dir,
        node_bin,
        npm_bin,
        gateway_entry,
        gateway_cwd,
    })
}

pub fn validate_runtime_layout(layout: &RuntimeLayout) -> Result<()> {
    ensure_exists(&layout.node_bin, "Node.js runtime 不存在")?;
    ensure_exists(&layout.gateway_entry, "openclaw gateway 入口不存在")?;
    ensure_exists(&layout.gateway_cwd, "openclaw gateway cwd 不存在")?;
    Ok(())
}

fn resolve_resources_dir() -> PathBuf {
    if let Ok(value) = std::env::var("ONECLAW_RESOURCES_DIR") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    let target = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("resources")
        .join("targets")
        .join(target)
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
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(path: &Path) -> Self {
            let prev = std::env::var("ONECLAW_RESOURCES_DIR").ok();
            std::env::set_var("ONECLAW_RESOURCES_DIR", path);
            Self { prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(prev) = &self.prev {
                std::env::set_var("ONECLAW_RESOURCES_DIR", prev);
            } else {
                std::env::remove_var("ONECLAW_RESOURCES_DIR");
            }
        }
    }

    #[test]
    #[serial(openclaw_env)]
    fn resolves_layout_from_env_resources_dir() {
        let dir = TempDir::new().unwrap();
        let _guard = EnvGuard::set(dir.path());
        let layout = resolve_runtime_layout().unwrap();
        assert_eq!(layout.resources_dir, dir.path());
        assert!(layout.node_bin.ends_with(if cfg!(windows) { "node.exe" } else { "node" }));
    }
}
