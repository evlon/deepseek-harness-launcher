//! dsh 版本管理：多版本共存、检查更新、切换版本。
//!
//! dsh（deepseek-harness-pkg）更新频繁，需要能：
//! - 列出已装版本 + GitHub/内网镜像的可用版本
//! - 下载指定版本（多源：内网镜像 → GitHub 官方 → ghfast.top 镜像）
//! - 切换激活版本（停 Harness → 替换 `dependencies/dsh` → 重启）
//!
//! 目录布局：
//! ```text
//! dependencies/
//!   dsh/                    ← 当前激活（所有现有代码引用此路径，保持不变）
//!   dsh-versions/
//!     <tag>/                ← 每个已装版本一个完整目录（如 v0.1.2-rc.3/）
//! ```
//!
//! 切换 = 把 `<tag>` 目录整体复制（原子替换）到 `dependencies/dsh`。
//! 复制而非符号链接：Windows junction 需要管理员权限，且 dsh 内含
//! node_modules 硬路径引用，完整目录副本最稳妥。

use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Runtime};

use crate::config::*;

/// 已装版本目录（`<base>/dsh-versions`）。
fn versions_dir<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    base_dir(app).join("dependencies").join("dsh-versions")
}

/// 某个版本对应的完整目录。
fn version_dir<R: Runtime>(app: &AppHandle<R>, tag: &str) -> PathBuf {
    versions_dir(app).join(tag)
}

/// 当前激活 dsh 目录（与 download.rs 的 `dsh_install_path` 一致）。
fn active_dir<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    dsh_install_path(app)
}

/// 读取一个版本目录的 package.json 版本号（形如 `0.1.1-rc.1`）。
/// 容忍 UTF-8 BOM（Windows 上 PowerShell/记事本写入可能带 BOM，serde_json 不认）。
fn version_from_dir(dir: &Path) -> Option<String> {
    let manifest = dir.join("package.json");
    let mut text = std::fs::read_to_string(&manifest).ok()?;
    // 剥离 UTF-8 BOM（\u{FEFF}）
    if text.starts_with('\u{FEFF}') {
        text = text.trim_start_matches('\u{FEFF}').to_string();
    }
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    json.get("version")?.as_str().map(|s| s.to_string())
}

/// 已安装的版本列表（按版本号倒序，最新的在前）。
/// 每项：{ tag: 目录名, version: package.json 版本号, active: 是否当前激活 }
/// 若 `dsh-versions/` 为空（升级前的旧安装），把当前激活 dsh 作为唯一版本列出，
/// 保证托盘「切换版本」菜单始终有内容、升级不破坏旧环境。
pub fn list_installed<R: Runtime>(app: &AppHandle<R>) -> Vec<serde_json::Value> {
    let dir = versions_dir(app);
    let mut out: Vec<serde_json::Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let tag = entry.file_name().to_string_lossy().to_string();
            let version = version_from_dir(&entry.path()).unwrap_or_default();
            out.push(serde_json::json!({
                "tag": tag,
                "version": version,
                "active": false,
            }));
        }
    }
    // 版本目录为空 → 旧安装：把当前激活 dsh 纳入（tag=v<version>）
    if out.is_empty() {
        let version = active_version(app);
        if !version.is_empty() {
            out.push(serde_json::json!({
                "tag": format!("v{version}"),
                "version": version,
                "active": true,
            }));
        }
    }
    out.sort_by(|a, b| {
        let va = a["version"].as_str().unwrap_or("");
        let vb = b["version"].as_str().unwrap_or("");
        // 倒序：新版在前
        semver_compare(vb, va)
    });
    out
}

/// 简单 semver 比较（a < b → Less，a > b → Greater）。
/// 处理 rc/beta 等预发布后缀（`0.1.1-rc.2` < `0.1.1`）。
fn semver_compare(a: &str, b: &str) -> std::cmp::Ordering {
    match (semver::Version::parse(a), semver::Version::parse(b)) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        _ => a.cmp(b),
    }
}

/// 当前激活版本的 tag（目录名；无版本目录时回退 "current"）。
pub fn active_tag<R: Runtime>(app: &AppHandle<R>) -> String {
    // 激活目录里读 package.json 版本，反查 dsh-versions 里的 tag
    let active = active_dir(app);
    if let Some(version) = version_from_dir(&active) {
        // 优先精确匹配版本号
        for v in list_installed(app) {
            if v["version"].as_str() == Some(version.as_str()) {
                return v["tag"].as_str().unwrap_or("current").to_string();
            }
        }
        // 版本号匹配不到 tag（如初次安装未走版本管理）→ 用版本号作 tag
        return format!("v{version}");
    }
    "current".to_string()
}

/// 当前激活版本号（`dependencies/dsh/package.json`）。
pub fn active_version<R: Runtime>(app: &AppHandle<R>) -> String {
    version_from_dir(&active_dir(app)).unwrap_or_default()
}

/// GitHub 上 deepseek-harness-pkg 的 release tag 列表（含资产 URL）。
/// 网络失败返回 Err——调用方降级为仅本地列表。
pub async fn fetch_remote_releases() -> Result<Vec<serde_json::Value>, String> {
    let client = reqwest::Client::builder()
        .user_agent("dsh-harness-launcher-version")
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|e| e.to_string())?;

    // 直连 GitHub API，失败走 ghfast.top 镜像
    let api_url = "https://api.github.com/repos/dsh-tauri-desk/deepseek-harness-pkg/releases?per_page=20";
    let mut urls = vec![api_url.to_string()];
    if let Ok(prefix) = std::env::var("GH_MIRROR") {
        if !prefix.is_empty() {
            urls.push(format!("{prefix}{api_url}"));
        }
    }
    urls.push(format!("https://ghfast.top/{api_url}"));

    let asset_name = dsh_asset_filename()?;
    let mut last_err = String::new();
    for url in &urls {
        match client.get(url).send().await {
            Ok(res) if res.status().is_success() => {
                let releases: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
                let arr = releases.as_array().ok_or("releases 非数组")?;
                let out: Vec<serde_json::Value> = arr
                    .iter()
                    .filter_map(|r| {
                        let tag = r.get("tag_name")?.as_str()?.to_string();
                        let prerelease = r.get("prerelease").and_then(|v| v.as_bool()).unwrap_or(false);
                        // 资产 URL：固定命名，按 tag 拼
                        let asset_url = format!(
                            "https://github.com/dsh-tauri-desk/deepseek-harness-pkg/releases/download/{tag}/{asset_name}"
                        );
                        Some(serde_json::json!({
                            "tag": tag,
                            "prerelease": prerelease,
                            "assetUrl": asset_url,
                        }))
                    })
                    .collect();
                log::info!("dsh 版本列表：从 {url} 拉到 {} 个 release", out.len());
                return Ok(out);
            }
            Ok(res) => last_err = format!("HTTP {}", res.status()),
            Err(e) => last_err = e.to_string(),
        }
    }
    Err(format!("FETCH_DSH_RELEASES_FAILED: {last_err}"))
}

/// dsh 平台资产文件名（与 download.rs 一致）。
fn dsh_asset_filename() -> Result<String, String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", _) => Ok("deepseek-harness-pkg-windows.zip".to_string()),
        ("macos", "aarch64") => Ok("deepseek-harness-pkg-macos-arm64.zip".to_string()),
        ("macos", "x86_64") => Ok("deepseek-harness-pkg-macos-x64.zip".to_string()),
        ("linux", _) => Ok("deepseek-harness-pkg-linux.zip".to_string()),
        other => Err(format!("不支持的平台：{:?}", other)),
    }
}

/// 下载并安装指定版本到 `dsh-versions/<tag>`（不切换激活）。
/// 下载源优先级：内网镜像（config.dshMirrorUrl）→ GitHub 官方 → ghfast.top 镜像。
pub async fn install_version<R: Runtime>(
    app: &AppHandle<R>,
    tag: &str,
    on_progress: crate::download::ProgressCallback<'_>,
) -> Result<(), String> {
    let cfg = load_cached();
    let dest = version_dir(app, tag);
    if dest.join("package.json").exists() {
        log::info!("dsh 版本 {tag} 已存在，跳过下载");
        return Ok(());
    }

    // 首次启用版本管理：把当前激活 dsh 备份进版本目录，避免旧版本"消失"
    ensure_active_backed_up(app);

    let asset_name = dsh_asset_filename()?;
    // 收集候选 URL：内网镜像 → GitHub 官方 → ghfast.top
    let mut urls: Vec<String> = Vec::new();
    // 1) 内网镜像（管理员配置，形如 http://registry.ict.cmcc/dsh/）
    if let Some(mirror) = dsh_mirror_base(&cfg) {
        urls.push(format!(
            "{}{}/{}",
            mirror.trim_end_matches('/'),
            tag.trim_start_matches('v'),
            asset_name
        ));
        urls.push(format!(
            "{}{}/{}",
            mirror.trim_end_matches('/'),
            tag,
            asset_name
        ));
    }
    // 2) GitHub 官方
    urls.push(format!(
        "https://github.com/dsh-tauri-desk/deepseek-harness-pkg/releases/download/{tag}/{asset_name}"
    ));
    // 3) ghfast.top 镜像
    urls.push(format!(
        "https://ghfast.top/https://github.com/dsh-tauri-desk/deepseek-harness-pkg/releases/download/{tag}/{asset_name}"
    ));

    log::info!("下载 dsh {tag}：候选源 {} 个（第一个内网镜像）", urls.len());
    let buf = crate::download::download_bytes(&urls, on_progress).await?;
    // 解压到版本目录（ensure_extract 自动摊平 + 原子切换）
    crate::download::ensure_extract(&asset_name, buf, dest.clone()).await?;
    let version = version_from_dir(&dest).unwrap_or_else(|| tag.to_string());
    log::info!("dsh 版本 {tag} 安装完成（package.json 版本 {version}）");
    Ok(())
}

/// 切换激活版本：停 Harness → 复制版本目录到 `dependencies/dsh` → 重启。
/// 返回 (旧版本号, 新版本号)。
pub async fn switch_version<R: Runtime>(
    app: &AppHandle<R>,
    tag: &str,
) -> Result<(String, String), String> {
    let src = version_dir(app, tag);
    if !src.join("package.json").exists() {
        return Err(format!("DSH_VERSION_NOT_INSTALLED: 版本 {tag} 未安装"));
    }

    // 1. 停 Harness（若在运行）
    // 判断口径：本进程记录的运行状态 OR 配置端口被占用（可能是其他实例启动的 Harness，
    // 如 CLI 模式看不到常驻实例的 RUNNING——按端口探测兜底，切换必须释放 dsh 目录锁）
    let cfg = load_cached();
    let port = crate::config::resolve_port(&cfg);
    let was_running = crate::workflow::is_running() || crate::workflow::port_in_use(port);
    if was_running {
        log::info!("切换 dsh 版本：先停止 Harness（端口 {port}）");
        crate::workflow::stop();
        // 等待进程退出释放文件锁（taskkill 异步）
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // 2. 复制版本目录 → 激活目录（原子替换）
    // 删除可能因 Harness 进程占用而失败（Windows 文件锁）——重试等待，
    // 进程退出后锁释放（真实场景 workflow::stop 已先杀进程树）。
    let old_version = active_version(app);
    let dest = active_dir(app);
    let mut cleaned = false;
    for attempt in 1..=5 {
        match crate::download::remove_path_if_exists(&dest).await {
            Ok(()) => {
                cleaned = true;
                break;
            }
            Err(e) => {
                log::warn!(
                    "清理激活 dsh 目录失败（第 {attempt}/5 次）：{e}；2s 后重试（可能是 Harness 进程占用）"
                );
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
    if !cleaned {
        return Err("DSH_SWAP_CLEAN_FAILED: 激活 dsh 目录被占用（Harness 未完全停止？），请重试".to_string());
    }
    copy_dir_all(&src, &dest).map_err(|e| format!("DSH_SWAP_COPY_FAILED: {e}"))?;
    let new_version = active_version(app);
    log::info!("dsh 版本切换完成：{old_version} -> {new_version}");

    // 3. 重启 Harness（若之前在运行）
    if was_running {
        match crate::workflow::launch(app) {
            Ok(port) => log::info!("切换后 Harness 已重启：http://127.0.0.1:{port}"),
            Err(e) => log::warn!("切换后 Harness 重启失败：{e}"),
        }
    }

    Ok((old_version, new_version))
}

/// 检查更新：当前激活版本 vs 远程最新 release。
/// 返回 (当前版本, 最新版本, 是否有更新)。
pub async fn check_update<R: Runtime>(app: &AppHandle<R>) -> (String, Option<String>, bool) {
    let current = active_version(app);
    let remote = match fetch_remote_releases().await {
        Ok(list) => list
            .first()
            .and_then(|r| r["tag"].as_str().map(|s| s.to_string())),
        Err(e) => {
            log::warn!("检查 dsh 更新失败：{e}");
            None
        }
    };
    let has_update = match (&remote, current.is_empty()) {
        (Some(remote_tag), false) => {
            // 远程 tag 形如 v0.1.2-rc.3；当前版本形如 0.1.1-rc.1
            let remote_ver = remote_tag.trim_start_matches('v');
            semver_compare(remote_ver, &current) == std::cmp::Ordering::Greater
        }
        (Some(_), true) => true, // 未安装 → 有更新
        _ => false,
    };
    (current, remote, has_update)
}

/// 复制目录（递归，覆盖已有）。
fn copy_dir_all(src: &Path, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::copy(&from, &to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// 内网 dsh 分发源（配置 `mirrorSettings.dshMirrorUrl` 或 `clientDefaults.dshMirrorUrl`）。
/// 形如 `http://registry.ict.cmcc/dsh/`；空 = 未配置，只用 GitHub。
fn dsh_mirror_base(cfg: &LauncherConfig) -> Option<String> {
    cfg.mirror_settings
        .as_ref()
        .and_then(|m| m.dsh_mirror_url.clone())
        .filter(|u| !u.is_empty())
        .or_else(|| {
            // clientDefaults 里也允许下发（服务器统一管理）
            None
        })
}

/// 首次启用版本管理时，把当前激活 dsh 备份进版本目录（tag=v<version>）。
/// 幂等：版本目录非空或当前版本已备份则跳过。
fn ensure_active_backed_up<R: Runtime>(app: &AppHandle<R>) {
    let version = active_version(app);
    if version.is_empty() {
        return;
    }
    let tag = format!("v{version}");
    let dest = version_dir(app, &tag);
    if dest.join("package.json").exists() {
        return; // 已备份
    }
    let active = active_dir(app);
    if !active.join("package.json").exists() {
        return;
    }
    log::info!("首次启用 dsh 版本管理：备份当前版本 {version} 到 {tag}");
    if let Err(e) = copy_dir_all(&active, &dest) {
        log::warn!("备份当前 dsh 版本失败（不影响继续）：{e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_compare_basics() {
        assert_eq!(semver_compare("0.1.1-rc.1", "0.1.2-rc.3"), std::cmp::Ordering::Less);
        assert_eq!(semver_compare("0.1.2", "0.1.1-rc.3"), std::cmp::Ordering::Greater);
        assert_eq!(semver_compare("1.0.0", "0.9.9"), std::cmp::Ordering::Greater);
        assert_eq!(semver_compare("0.1.1", "0.1.1"), std::cmp::Ordering::Equal);
    }

    #[test]
    fn asset_filename_windows() {
        assert_eq!(dsh_asset_filename().unwrap(), "deepseek-harness-pkg-windows.zip");
    }
}
