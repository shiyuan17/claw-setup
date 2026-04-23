use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

const MAX_LOG_SIZE: u64 = 5 * 1024 * 1024;
const ROTATION_CHECK_INTERVAL: usize = 1000;

static WRITE_COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn app_log_path() -> PathBuf {
    crate::config::state_dir().join("app.log")
}

pub fn info(message: impl AsRef<str>) {
    write("INFO", message.as_ref());
}

pub fn warn(message: impl AsRef<str>) {
    write("WARN", message.as_ref());
}

pub fn error(message: impl AsRef<str>) {
    write("ERROR", message.as_ref());
}

fn write(level: &str, message: &str) {
    let line = format!("[{}] [{}] {}\n", chrono::Utc::now().to_rfc3339(), level, message);
    if let Some(parent) = app_log_path().parent() {
        let _ = fs::create_dir_all(parent);
    }

    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(app_log_path())
    {
        let _ = file.write_all(line.as_bytes());
    }

    if WRITE_COUNT.fetch_add(1, Ordering::Relaxed) % ROTATION_CHECK_INTERVAL == 0 {
        rotate_if_needed();
    }

    if level == "ERROR" {
        eprint!("{line}");
    } else {
        print!("{line}");
    }
}

fn rotate_if_needed() {
    let path = app_log_path();
    let Ok(metadata) = fs::metadata(&path) else {
        return;
    };
    if metadata.len() <= MAX_LOG_SIZE {
        return;
    }
    let _ = fs::write(path, "[truncated]\n");
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serial_test::serial;
    use tempfile::TempDir;

    use super::*;

    struct EnvGuard {
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(path: &Path) -> Self {
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
    fn writes_app_log_into_state_dir() {
        let dir = TempDir::new().unwrap();
        let _guard = EnvGuard::set(dir.path());
        info("hello");
        let raw = fs::read_to_string(dir.path().join("app.log")).unwrap();
        assert!(raw.contains("[INFO] hello"));
    }
}
