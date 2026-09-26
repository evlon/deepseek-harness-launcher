//! UI 包：服务端下发的本地窗口 HTML（在线优先 / 离线回落内置）。
//!
//! ## 背景
//! launcher 三个本地窗口（数字分身激活向导 / 首次欢迎 / 操作进度）的 HTML 原先
//! 编译期内嵌（`include_str!`，见 `embedded.rs`）——**任何 UI 改动都要重编译 exe 发版**。
//! 本模块实现「双轨加载」：
//!
//! ```text
//! 窗口 HTML 请求
//!   ├─ 1. 本地 UI 包缓存（app_data_dir/ui-bundle/v<版本>/*.html）→ 命中即用（在线版）
//!   └─ 2. 未命中/未下载 → 回落编译期内嵌（embedded.rs，内置兜底）
//! ```
//!
//! 版本来源：服务端 `/api/config` 返回 `uiBundle.version`（元数据，小）；launcher 同步时
//! 发现本地缓存版本落后 → 主动 `GET /api/ui-bundle`（客户端主动发起，服务端从不推送）
//! → 写入缓存目录。窗口加载不需要网络——缓存文件在本地。
//!
//! ## 安全边界（与「客户端主动发起才可调本地能力」原则一致）
//! - 只在「同步周期内」主动拉取 UI 包（客户端单向出站，无任何入站监听）
//! - 内容严格限定：`files` 键必须是 `*.html`、单文件 ≤512KB、必须是 HTML 文档
//! - 版本号白名单字符（字母/数字/./-/），防止路径穿越
//! - 本地缓存仅 launcher 自己写入（用户 app_data 目录，管理员/本机可信）
//! - 加载时逐文件校验存在性；损坏/缺失 → 回落内置，不抛错不弹窗

use serde::Deserialize;
use std::path::PathBuf;
use tauri::{AppHandle, Runtime};

/// 单文件大小上限（与服务端校验一致）。
pub const MAX_FILE_BYTES: usize = 512 * 1024;
/// 版本号合法字符。
fn valid_version(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 100
        && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// 服务端 /api/ui-bundle 响应。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UiBundle {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub files: std::collections::HashMap<String, String>,
}

/// 本地缓存目录：app_data_dir/ui-bundle/v<版本>/
pub fn cache_dir<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    crate::config::base_dir(app).join("ui-bundle")
}

/// 目标缓存目录（按版本）。
fn version_dir<R: Runtime>(app: &AppHandle<R>, version: &str) -> Option<PathBuf> {
    if !valid_version(version) {
        return None;
    }
    Some(cache_dir(app).join(format!("v{version}")))
}

/// 下载并缓存 UI 包（同步流程里后台调用）。返回缓存到的版本；失败不报错只记日志。
pub async fn download_ui_bundle<R: Runtime>(app: &AppHandle<R>, server_url: &str, version: &str) -> Result<String, String> {
    if !valid_version(version) {
        return Err(format!("UI 包版本号非法: {version}"));
    }
    let url = format!("{}/api/ui-bundle", server_url.trim_end_matches('/'));
    let client = crate::sync::http_client();
    let resp = client.get(&url).send().await.map_err(|e| format!("拉取 UI 包失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("UI 包 HTTP {}", resp.status()));
    }
    let bundle: UiBundle = resp.json().await.map_err(|e| format!("解析 UI 包失败: {e}"))?;
    if bundle.version != version {
        return Err(format!("UI 包版本不符（要 {version}，服务端返回 {}）", bundle.version));
    }
    if bundle.files.is_empty() {
        return Err("UI 包无文件".to_string());
    }
    let dir = version_dir(app, &bundle.version).ok_or_else(|| "UI 包版本非法".to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建缓存目录失败: {e}"))?;
    let mut wrote = 0usize;
    for (name, content) in &bundle.files {
        // 键名白名单：仅 *.html，防路径穿越
        if !name.ends_with(".html") || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')) {
            log::warn!("UI 包跳过非法文件名: {name}");
            continue;
        }
        if content.len() > MAX_FILE_BYTES {
            log::warn!("UI 包跳过超大文件: {name} ({} bytes)", content.len());
            continue;
        }
        if !content.contains("<html") {
            log::warn!("UI 包跳过非 HTML 文件: {name}");
            continue;
        }
        let path = dir.join(name);
        std::fs::write(&path, content).map_err(|e| format!("写 UI 包失败: {e}"))?;
        wrote += 1;
    }
    if wrote == 0 {
        return Err("UI 包无可写文件".to_string());
    }
    log::info!("UI 包已缓存 v{}（{} 个文件）", bundle.version, wrote);
    prune_old_versions(app, &bundle.version);
    Ok(bundle.version)
}

/// 清理旧版本缓存目录，避免 UI 包版本号无限累积（每次发版遗留一个 v<版本>/）。
/// 只删除「非当前版本」的目录；当前版本刚写完，保留。失败仅记日志，不影响主流程。
fn prune_old_versions<R: Runtime>(app: &AppHandle<R>, keep_version: &str) {
    prune_old_versions_in_dir(&cache_dir(app), keep_version)
}

/// prune_old_versions 的纯函数版（测试友好）。
fn prune_old_versions_in_dir(root: &PathBuf, keep_version: &str) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        // 目录名形如 v<版本>，剥掉 v 前缀后与当前版本比较；不同则删除
        let Some(stripped) = name.strip_prefix('v') else {
            continue;
        };
        if stripped != keep_version {
            log::info!("UI 包清理旧版本目录: {name}");
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// 双轨加载：优先读缓存文件，无则回落内置。返回 (html, 来源)。
/// 来源用于日志：`cache@v<版本>` 或 `builtin`。
pub fn load_html<R: Runtime>(app: &AppHandle<R>, name: &str, builtin: &str) -> (String, String) {
    load_html_from_dir(cache_dir(app), name, builtin)
}

/// 双轨加载的纯函数版（测试友好）：从指定缓存根目录读取。
/// `cache_root` = ui-bundle 目录（其下 v<版本>/<name>.html）。
/// 从**最大版本往下**逐个试：某版本目录存在但缺文件 → 继续检查旧版本，
/// 全部没有才回落内置（避免「高版本目录存在但文件缺失」时误回退）。
pub fn load_html_from_dir(cache_root: PathBuf, name: &str, builtin: &str) -> (String, String) {
    let fname = if name.ends_with(".html") { name.to_string() } else { format!("{name}.html") };
    let mut versions = versions_in_dir(&cache_root);
    // 从大到小
    versions.sort_by(|a, b| b.cmp(a));
    for ver in &versions {
        let path = cache_root.join(format!("v{ver}")).join(&fname);
        if path.is_file() {
            match std::fs::read_to_string(&path) {
                Ok(content) if content.contains("<html") => {
                    log::debug!("窗口 HTML 用服务端下发版: {fname} (v{ver})");
                    return (content, format!("cache@v{ver}"));
                }
                _ => log::debug!("窗口 HTML 缓存缺失/无效: {fname} (v{ver})，继续检查旧版本"),
            }
        }
    }
    (builtin.to_string(), "builtin".to_string())
}

/// 目录内全部版本（纯函数，测试友好）。
fn versions_in_dir(dir: &PathBuf) -> Vec<String> {
    std::fs::read_dir(dir)
        .ok()
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().to_string();
                    n.strip_prefix('v').map(|s| s.to_string())
                })
                .filter(|s| valid_version(s))
                .collect()
        })
        .unwrap_or_default()
}

/// 目录内最大版本（纯函数，测试友好）。
fn latest_version_in_dir(dir: &PathBuf) -> Option<String> {
    let mut versions = versions_in_dir(dir);
    versions.sort_by(|a, b| b.cmp(a));
    versions.first().map(|s| s.to_string())
}

/// 当前缓存版本（读目录名，若无则空）。
pub fn cached_version<R: Runtime>(app: &AppHandle<R>) -> Option<String> {
    latest_version_in_dir(&cache_dir(app))
}

/// 需要更新？(本地缓存版本 vs 服务端下发版本)
pub fn needs_update(server_version: &str, cached: Option<&str>) -> bool {
    if server_version.is_empty() {
        return false; // 服务端未启用 UI 包
    }
    match cached {
        Some(c) => c != server_version,
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_validation() {
        assert!(valid_version("20260927.1"));
        assert!(valid_version("v1.2.3"));
        assert!(!valid_version(""));
        assert!(!valid_version("../../etc"));
        assert!(!valid_version("a b"));
        assert!(!valid_version(&"x".repeat(101)));
    }

    #[test]
    fn needs_update_logic() {
        assert!(!needs_update("", None));
        assert!(!needs_update("", Some("1")));
        assert!(needs_update("20260927.1", None));
        assert!(needs_update("20260927.1", Some("20260926.1")));
        assert!(!needs_update("20260927.1", Some("20260927.1")));
    }

    #[test]
    fn load_html_uses_cache_when_present() {
        // 造临时缓存目录：v20260927.1/matrix-setup.html
        let dir = std::env::temp_dir().join(format!("ui-bundle-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join("v20260927.1"));
        // 清理旧内容
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        std::fs::write(dir.join("v20260927.1").join("matrix-setup.html"), "<html><body>服务端下发版</body></html>").unwrap();

        let builtin = "<html><body>内置版</body></html>";
        // 命中缓存
        let (html, src) = load_html_from_dir(dir.clone(), "matrix-setup.html", builtin);
        assert_eq!(html, "<html><body>服务端下发版</body></html>");
        assert_eq!(src, "cache@v20260927.1");
        // 无缓存文件 → 回落内置
        let (html2, src2) = load_html_from_dir(dir.clone(), "nope.html", builtin);
        assert_eq!(html2, builtin);
        assert_eq!(src2, "builtin");
        // latest_version_in_dir 取最大版本
        let _ = std::fs::create_dir_all(dir.join("v20260928.0"));
        let (_html3, src3) = load_html_from_dir(dir.clone(), "matrix-setup.html", builtin);
        assert_eq!(src3, "cache@v20260927.1"); // v20260928.0 无文件 → 仍取缓存目录里存在的文件版本
        let _ = std::fs::write(dir.join("v20260928.0").join("matrix-setup.html"), "<html>v28</html>");
        let (html4, src4) = load_html_from_dir(dir.clone(), "matrix-setup.html", builtin);
        assert_eq!(src4, "cache@v20260928.0");
        assert_eq!(html4, "<html>v28</html>");
        // 清理
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_html_skips_invalid_cache_content() {
        let dir = std::env::temp_dir().join(format!("ui-bundle-test-invalid-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join("v1"));
        // 坏文件（不含 <html 的正文）→ 不采用
        std::fs::write(dir.join("v1").join("console.html"), "not-html").unwrap();
        let builtin = "<html>builtin</html>";
        let (html, src) = load_html_from_dir(dir.clone(), "console.html", builtin);
        assert_eq!(src, "builtin");
        assert_eq!(html, builtin);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_old_versions_keeps_only_current() {
        let dir = std::env::temp_dir().join(format!("ui-bundle-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // 造三个版本目录 + 一个非版本目录
        std::fs::create_dir_all(dir.join("v1")).unwrap();
        std::fs::create_dir_all(dir.join("v2")).unwrap();
        std::fs::create_dir_all(dir.join("v3")).unwrap();
        std::fs::create_dir_all(dir.join("backup-notes")).unwrap();
        std::fs::write(dir.join("v1").join("console.html"), "<html>1</html>").unwrap();
        std::fs::write(dir.join("v2").join("console.html"), "<html>2</html>").unwrap();
        std::fs::write(dir.join("v3").join("console.html"), "<html>3</html>").unwrap();

        // 保留 v3，清理 v1/v2；非 v 前缀目录（backup-notes）不碰
        prune_old_versions_in_dir(&dir, "3");

        assert!(!dir.join("v1").exists(), "旧版本 v1 应被清理");
        assert!(!dir.join("v2").exists(), "旧版本 v2 应被清理");
        assert!(dir.join("v3").exists(), "当前版本 v3 应保留");
        assert!(dir.join("backup-notes").exists(), "非版本目录不应被误删");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
