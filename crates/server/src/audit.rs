use chrono::Local;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing::info;

const MAX_LOG_SIZE: u64 = 100 * 1024 * 1024; // 100 MB
const MAX_LOG_FILES: usize = 5;

/// Simple append-only audit logger that writes JSON-lines entries to a file.
/// Supports automatic rotation when the log exceeds MAX_LOG_SIZE.
pub struct AuditLog {
    path: PathBuf,
    file: Mutex<std::fs::File>,
}

impl AuditLog {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        info!("Audit log opened: {}", path.display());
        Ok(Self {
            path: path.to_path_buf(),
            file: Mutex::new(file),
        })
    }

    pub fn log(&self, action: &str, detail: &str) {
        // Sanitize to prevent log injection
        let sanitized_action = action.replace(['\n', '\r'], " ");
        let sanitized_detail = detail.replace(['\n', '\r'], " ");

        let ts = Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%z").to_string();

        // Write as JSON line
        if let Ok(json) = serde_json::to_string(&serde_json::json!({
            "ts": ts,
            "action": sanitized_action,
            "detail": sanitized_detail,
        })) {
            if let Ok(mut f) = self.file.lock() {
                let _ = writeln!(f, "{json}");
            }
        }

        self.maybe_rotate();
    }

    pub fn log_connect(&self, addr: &str) {
        self.log("CONNECT", addr);
    }

    pub fn log_disconnect(&self, addr: &str) {
        self.log("DISCONNECT", addr);
    }

    pub fn log_command(&self, addr: &str, command: &str) {
        self.log("COMMAND", &format!("{addr} -> {command}"));
    }

    fn maybe_rotate(&self) {
        let needs_rotation = std::fs::metadata(&self.path)
            .map(|m| m.len() > MAX_LOG_SIZE)
            .unwrap_or(false);

        if !needs_rotation {
            return;
        }

        if let Ok(mut f) = self.file.lock() {
            // Rotate: audit.log.4 deleted, audit.log.3 -> .4, ... audit.log -> .1
            for i in (1..MAX_LOG_FILES).rev() {
                let from = format!("{}.{i}", self.path.display());
                let to = format!("{}.{}", self.path.display(), i + 1);
                let _ = std::fs::rename(&from, &to);
            }
            let backup = format!("{}.1", self.path.display());
            let _ = std::fs::rename(&self.path, &backup);

            if let Ok(new_file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
            {
                *f = new_file;
                info!("Audit log rotated");
            }
        }
    }
}
