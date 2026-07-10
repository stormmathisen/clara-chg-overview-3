use chrono::Local;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing::info;

const MAX_LOG_SIZE: u64 = 100 * 1024 * 1024; // 100 MB
const MAX_LOG_FILES: usize = 5;

/// Simple append-only audit logger that writes JSON-lines entries to a file.
/// Supports automatic rotation when the log exceeds `max_size`.
pub struct AuditLog {
    path: PathBuf,
    file: Mutex<std::fs::File>,
    /// Rotate once the log grows beyond this many bytes.
    max_size: u64,
}

impl AuditLog {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        Self::open_with_max_size(path, MAX_LOG_SIZE)
    }

    fn open_with_max_size(path: &Path, max_size: u64) -> anyhow::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        info!("Audit log opened: {}", path.display());
        Ok(Self {
            path: path.to_path_buf(),
            file: Mutex::new(file),
            max_size,
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
            .map(|m| m.len() > self.max_size)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_writes_sanitized_json_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let audit = AuditLog::open(&path).unwrap();
        audit.log_command("ws-client", "line1\nline2\rline3");

        let contents = std::fs::read_to_string(&path).unwrap();
        // One JSON line, newlines/carriage-returns replaced with spaces.
        assert_eq!(contents.lines().count(), 1);
        assert!(contents.contains("line1 line2 line3"));
        assert!(!contents.trim_end().contains('\n'));
        let parsed: serde_json::Value = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(parsed["action"], "COMMAND");
    }

    #[test]
    fn exceeding_max_size_rotates_to_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        // Tiny threshold so a couple of entries trigger rotation.
        let audit = AuditLog::open_with_max_size(&path, 64).unwrap();
        for _ in 0..20 {
            audit.log(
                "COMMAND",
                "some reasonably long detail string to grow the file",
            );
        }
        // The rotated-out file exists and the live log has been recreated.
        let backup = path.with_file_name("audit.log.1");
        assert!(backup.exists(), "expected rotated backup at {backup:?}");
        assert!(path.exists());
    }
}
