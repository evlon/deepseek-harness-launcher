//! 排障日志收集：一键打包关键日志与配置摘要到桌面 zip，便于同事回传。
//!
//! 背景（同事实测）：启动失败（如 HARNESS_NOT_READY）时，日志分散在
//! `%APPDATA%\...\logs\`（launcher 日志）与 `~/.dsh-launcher\`（dsh 启动日志、
//! 诊断日志、配置）两处，小白不知道该发哪个文件——收集成一个 zip 最省事。
//!
//! 脱敏：accessToken / password / token 等敏感值只保留前 4 位 + 掩码。

use std::io::Write;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Runtime};

use crate::config::*;

/// 收集结果（zip 路径 + 包含的文件数）。
pub struct PackResult {
    pub zip_path: PathBuf,
    pub file_count: usize,
}

/// 收集日志与配置摘要，打包到桌面。
pub fn collect<R: Runtime>(app: &AppHandle<R>) -> Result<PackResult, String> {
    let cfg = load_cached();
    let home = dsh_home(app, &cfg);
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let zip_path = desktop_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("dsh-launcher-logs-{stamp}.zip"));

    let file = std::fs::File::create(&zip_path).map_err(|e| format!("创建 zip 失败：{e}"))?;
    let mut zip = zip::ZipWriter::new(file);
    let opts: zip::write::FileOptions =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let mut count = 0usize;

    // ① launcher 日志（%APPDATA%\...\logs\launcher.log）
    let launcher_log = log_file(app);
    if launcher_log.is_file() {
        count += add_file(&mut zip, &launcher_log, "launcher.log", opts)?;
    }

    // ② dsh 启动日志 + 诊断日志 + 关键状态文件（~/.dsh-launcher\）
    let dsh_logs = home.join("logs");
    if let Ok(rd) = std::fs::read_dir(&dsh_logs) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file() {
                // dsh-launch-*.log：可能较大，只保留尾部（最近 2000 行）
                let name = format!("dsh-logs/{}", e.file_name().to_string_lossy());
                count += add_file_tail(&mut zip, &p, &name, 2000, opts)?;
            }
        }
    }
    // 诊断日志（dsh-matrix-agent 的 fileLog 输出，含插件加载/连接过程）
    let diag = home.join(".dsh-matrix").join("diagnostics.log");
    if diag.is_file() {
        count += add_file_tail(&mut zip, &diag, "dsh-matrix-diagnostics.log", 3000, opts)?;
    }

    // ③ 配置摘要（脱敏）
    let mut summary = String::new();
    summary.push_str(&format!("launcher 版本: {}\n", env!("CARGO_PKG_VERSION")));
    summary.push_str(&format!("DSH_HOME: {}\n", home.display()));
    summary.push_str(&format!("profile: {}\n", resolve_profile(&cfg)));
    summary.push_str(&format!("port: {}\n", resolve_port(&cfg)));
    summary.push_str(&format!("serverUrl: {}\n", resolve_server_url(&cfg)));
    summary.push_str(&format!(
        "npmRegistry: {:?}\n",
        cfg.npm_registry.clone().unwrap_or_default()
    ));
    summary.push_str(&format!("管理能力: {}\n", bridge_enabled(&cfg)));
    summary.push_str("\n--- settings.yaml（脱敏）---\n");
    let settings_path = home.join("settings.yaml");
    if let Ok(text) = std::fs::read_to_string(&settings_path) {
        summary.push_str(&redact(&text));
    } else {
        summary.push_str("（无 settings.yaml）\n");
    }
    summary.push_str("\n--- sync-state.json ---\n");
    let sync_path = home.join("sync-state.json");
    if let Ok(text) = std::fs::read_to_string(&sync_path) {
        summary.push_str(&redact(&text));
    }
    summary.push_str("\n--- ops-state.json（最近操作结果）---\n");
    let ops_path = home.join("ops-state.json");
    if let Ok(text) = std::fs::read_to_string(&ops_path) {
        summary.push_str(&redact(&text));
    }
    zip.start_file("summary.txt", opts).map_err(|e| e.to_string())?;
    zip.write_all(summary.as_bytes()).map_err(|e| e.to_string())?;
    count += 1;

    // ④ dsh 运行环境快照（端口占用等）——帮助判断端口冲突
    let env_info = format!(
        "port {} 占用: {}\nprofile 目录存在: {}\ndsh bin 存在: {}\n",
        resolve_port(&cfg),
        crate::workflow::port_in_use(resolve_port(&cfg)),
        home.join("profiles").join(resolve_profile(&cfg)).exists(),
        dsh_binary_path(app).exists(),
    );
    zip.start_file("env.txt", opts).map_err(|e| e.to_string())?;
    zip.write_all(env_info.as_bytes()).map_err(|e| e.to_string())?;
    count += 1;

    zip.finish().map_err(|e| format!("完成 zip 失败：{e}"))?;
    Ok(PackResult { zip_path, file_count: count })
}

/// 写入一个文件（整份）。
fn add_file(
    zip: &mut zip::ZipWriter<std::fs::File>,
    src: &Path,
    name: &str,
    opts: zip::write::FileOptions,
) -> Result<usize, String> {
    let Ok(bytes) = std::fs::read(src) else {
        return Ok(0);
    };
    zip.start_file(name, opts).map_err(|e| e.to_string())?;
    zip.write_all(&bytes).map_err(|e| e.to_string())?;
    Ok(1)
}

/// 写入文件尾部 n 行（大日志避免 zip 过大）。
fn add_file_tail(
    zip: &mut zip::ZipWriter<std::fs::File>,
    src: &Path,
    name: &str,
    n: usize,
    opts: zip::write::FileOptions,
) -> Result<usize, String> {
    let Ok(text) = std::fs::read_to_string(src) else {
        return Ok(0);
    };
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    let tail = lines[start..].join("\n");
    zip.start_file(name, opts).map_err(|e| e.to_string())?;
    zip.write_all(tail.as_bytes()).map_err(|e| e.to_string())?;
    Ok(1)
}

/// 敏感值脱敏：accessToken / password / token 的值只留前 4 位。
fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let lower = line.to_lowercase();
        let is_secret = lower.contains("accesstoken")
            || lower.contains("password")
            || lower.contains("tokenvalue")
            || lower.contains("admintoken")
            || lower.trim_start().starts_with("token:");
        if is_secret {
            if let Some((k, v)) = line.split_once(':') {
                let v = v.trim().trim_matches(|c| c == '\'' || c == '"');
                let shown = if v.len() > 4 { &v[..4] } else { "" };
                out.push_str(&format!("{k}: {shown}****（已脱敏）\n"));
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// 桌面目录（Windows 中文系统常见「桌面」；兼容 OneDrive 重定向）。
fn desktop_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    for name in ["Desktop", "桌面"] {
        let p = home.join(name);
        if p.is_dir() {
            return Some(p);
        }
    }
    // OneDrive 重定向
    let onedrive = home.join("OneDrive");
    for name in ["Desktop", "桌面"] {
        let p = onedrive.join(name);
        if p.is_dir() {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_hides_secrets() {
        let text = "dsh-matrix:\n  accessToken: LZk2VBXIYZTrrzn4Cf4N5GrGy_f7A6YNu3YfY5xpsXo\n  userId: '@ai-x:s'\n";
        let out = redact(text);
        assert!(out.contains("LZk2****"), "应保留前 4 位并掩码：{out}");
        assert!(!out.contains("LZk2VBXIYZTrrzn4"), "完整 token 不应出现");
        assert!(out.contains("userId: '@ai-x:s'"), "非敏感字段应原样保留");
    }

    #[test]
    fn redact_handles_password_and_token_keys() {
        let text = "password: 'secret123'\ntokenValue: test-token-12345\nadminToken: abcdef\n";
        let out = redact(text);
        assert!(out.contains("secr****"), "password 值应保留前 4 位掩码：{out}");
        assert!(!out.contains("secret123"));
        assert!(!out.contains("test-token-12345"));
        assert!(!out.contains("abcdef"));
    }
}
