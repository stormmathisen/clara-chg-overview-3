use chrono::Local;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing::info;

/// Simple append-only audit logger that writes timestamped entries to a file.
pub struct AuditLog {
    path: PathBuf,
    file: Mutex<std::fs::File>,
}

impl AuditLog {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        info!("Audit log opened: {}", path.display());
        Ok(Self {
            path: path.to_path_buf(),
            file: Mutex::new(file),
        })
    }

    pub fn log(&self, action: &str, detail: &str) {
        let ts = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let line = format!("[{ts}] {action}: {detail}\n");
        if let Ok(mut f) = self.file.lock() {
            let _ = f.write_all(line.as_bytes());
        }
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
}
