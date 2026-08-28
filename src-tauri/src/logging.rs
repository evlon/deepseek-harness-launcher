//! 极简文件日志：追加写入 `<logs>/launcher.log`，同时打印到 stdout。
//! 无 webview，因此不走 tauri-plugin-log 的前端通道。

use chrono::Local;
use log::{LevelFilter, Metadata, Record};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

struct FileLogger {
    file: Mutex<File>,
}

impl FileLogger {
    fn new(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }
}

impl log::Log for FileLogger {
    fn enabled(&self, _: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        let ts = Local::now().format("%Y-%m-%d %H:%M:%S");
        let line = format!("[{ts}] {}: {}", record.level(), record.args());
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{line}");
        }
        println!("{line}");
    }

    fn flush(&self) {
        if let Ok(mut f) = self.file.lock() {
            let _ = f.flush();
        }
    }
}

/// 初始化全局日志（文件 + stdout），失败仅告警不阻断。
pub fn init(path: &Path) {
    match FileLogger::new(path) {
        Ok(logger) => {
            if log::set_boxed_logger(Box::new(logger)).is_ok() {
                log::set_max_level(LevelFilter::Info);
            }
        }
        Err(e) => {
            eprintln!("Failed to init file logger: {e}");
            // 退化为仅 stdout
            let _ = env_logger_like_stdout();
        }
    }
}

/// 无文件时的兜底：仅打印到 stdout。
fn env_logger_like_stdout() -> bool {
    log::set_logger(&StdoutLogger).map(|()| log::set_max_level(LevelFilter::Info)).is_ok()
}

struct StdoutLogger;

impl log::Log for StdoutLogger {
    fn enabled(&self, _: &Metadata) -> bool {
        true
    }
    fn log(&self, record: &Record) {
        println!("[{}] {}: {}", Local::now().format("%H:%M:%S"), record.level(), record.args());
    }
    fn flush(&self) {}
}
