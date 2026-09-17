//! 极简文件日志：追加写入 `<logs>/launcher.log`，同时打印到 stdout。
//! 无 webview，因此不走 tauri-plugin-log 的前端通道。
//!
//! 日志轮转：每次启动前把上一个 `launcher.log` 归档为 `launcher-<时间戳>.log`，
//! 然后新建空 `launcher.log` 作为本次会话日志；归档文件只保留最近 3 个，
//! 更旧的自动删除（同事每次重启一个日志文件、最多保留 3 个）。

use chrono::Local;
use log::{LevelFilter, Metadata, Record};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
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
///
/// 在打开日志文件前先做轮转：把上一个 `launcher.log` 归档为带时间戳的文件，
/// 清理归档目录只保留最近 `MAX_ARCHIVED_LOGS` 个，再新建本次会话的 `launcher.log`。
pub fn init(path: &Path) {
    rotate_logs(path);
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

/// 归档日志最多保留数量。
const MAX_ARCHIVED_LOGS: usize = 3;

/// 日志归档文件前缀（不含时间戳与扩展名）。
const ARCHIVE_PREFIX: &str = "launcher-";

/// 日志轮转：
/// 1. 若 `launcher.log` 已存在且非空，改名为 `launcher-<mtime时间戳>.log`；
/// 2. 列出所有 `launcher-*.log`，按文件名（时间戳）排序，删除超出最近 3 个的旧文件。
fn rotate_logs(current: &Path) {
    // ① 归档本次会话之前的 launcher.log
    if current.is_file() {
        if let Ok(meta) = std::fs::metadata(current) {
            if meta.len() > 0 {
                if let Ok(modified) = meta.modified() {
                    let ts = format_archive_stamp(modified);
                    let archive = archive_path(current, &ts);
                    let _ = std::fs::rename(current, &archive);
                }
            } else {
                // 空文件（上次启动未写任何日志）：直接删，避免积累空归档
                let _ = std::fs::remove_file(current);
            }
        }
    }

    // ② 清理归档，只保留最近 MAX_ARCHIVED_LOGS 个
    prune_archives(current);
}

/// 将修改时间格式化为归档时间戳 `YYYYMMDD-HHMMSS`。
fn format_archive_stamp(modified: std::time::SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Local> = modified.into();
    dt.format("%Y%m%d-%H%M%S").to_string()
}

/// 构造归档文件路径：与当前日志同目录，命名为 `launcher-<ts>.log`。
fn archive_path(current: &Path, ts: &str) -> PathBuf {
    let dir = current.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!("{ARCHIVE_PREFIX}{ts}.log"))
}

/// 列出并清理归档日志，只保留最近 `MAX_ARCHIVED_LOGS` 个。
fn prune_archives(current: &Path) {
    let dir = match current.parent() {
        Some(d) => d,
        None => return,
    };
    let Ok(rd) = std::fs::read_dir(dir) else { return };

    // 收集所有 launcher-*.log（排除正在使用的 launcher.log）
    let mut archives: Vec<PathBuf> = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with(ARCHIVE_PREFIX) && name.ends_with(".log") {
            archives.push(e.path());
        }
    }

    if archives.len() <= MAX_ARCHIVED_LOGS {
        return;
    }

    // 按文件名倒序（时间戳字符串可字典序比较），保留前 3 个，删除其余
    archives.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    for old in archives.iter().skip(MAX_ARCHIVED_LOGS) {
        let _ = std::fs::remove_file(old);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 在临时目录里创建几个带不同时间戳的归档文件，验证只保留最近 3 个。
    #[test]
    fn prune_archives_keeps_most_recent_three() {
        let dir = std::env::temp_dir().join(format!("dsh-log-rot-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let current = dir.join("launcher.log");
        // 造 5 个归档文件（时间戳递增）
        for (i, ts) in [
            "20260916-100000",
            "20260916-110000",
            "20260916-120000",
            "20260916-130000",
            "20260916-140000",
        ]
        .iter()
        .enumerate()
        {
            let p = dir.join(format!("launcher-{ts}.log"));
            fs::write(&p, format!("log{i}")).unwrap();
        }
        // 顺带放一个非归档文件，确保不被误删
        fs::write(dir.join("settings.yaml"), "x").unwrap();

        prune_archives(&current);

        let mut remaining: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("launcher-") && n.ends_with(".log"))
            .collect();
        remaining.sort();

        assert_eq!(
            remaining,
            vec![
                "launcher-20260916-120000.log",
                "launcher-20260916-130000.log",
                "launcher-20260916-140000.log",
            ],
            "应只保留最近 3 个归档"
        );
        assert!(dir.join("settings.yaml").exists(), "非日志文件不应被删");

        let _ = fs::remove_dir_all(&dir);
    }

    /// 归档文件不足 3 个时不删任何东西。
    #[test]
    fn prune_archives_noop_when_fewer_than_three() {
        let dir = std::env::temp_dir().join(format!("dsh-log-rot-test2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let current = dir.join("launcher.log");
        fs::write(dir.join("launcher-20260916-100000.log"), "a").unwrap();
        fs::write(dir.join("launcher-20260916-110000.log"), "b").unwrap();

        prune_archives(&current);

        let count = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.starts_with("launcher-") && n.ends_with(".log")
            })
            .count();
        assert_eq!(count, 2, "不足 3 个归档时不应删除");

        let _ = fs::remove_dir_all(&dir);
    }

    /// 存在非空的 launcher.log 时，rotate_logs 会把它归档成带时间戳的文件。
    #[test]
    fn rotate_archives_previous_log() {
        let dir = std::env::temp_dir().join(format!("dsh-log-rot-test3-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let current = dir.join("launcher.log");
        fs::write(&current, "上次会话的日志内容").unwrap();

        rotate_logs(&current);

        // 原 launcher.log 应已被移走（归档或删除），且出现一个 launcher-*.log
        assert!(!current.exists(), "旧的 launcher.log 应被归档移走");
        let archived = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("launcher-") && n.ends_with(".log"))
            .count();
        assert_eq!(archived, 1, "应产生 1 个归档文件");

        let _ = fs::remove_dir_all(&dir);
    }
}
