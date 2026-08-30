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

/// 读取一个版本目录的版本号。
/// - 旧 GitHub 版：顶层 package.json 有 version（deepseek-harness-pkg 薄壳）
/// - npm 版：顶层是 dsh-runtime 壳（无 version），从 node_modules/@deepseek-ai/dsh/package.json 读
/// 容忍 UTF-8 BOM（Windows 上 PowerShell/记事本写入可能带 BOM，serde_json 不认）。
fn version_from_dir(dir: &Path) -> Option<String> {
    let read_version = |manifest: &Path| -> Option<String> {
        let mut text = std::fs::read_to_string(manifest).ok()?;
        if text.starts_with('\u{FEFF}') {
            text = text.trim_start_matches('\u{FEFF}').to_string();
        }
        let json: serde_json::Value = serde_json::from_str(&text).ok()?;
        json.get("version")?.as_str().map(|s| s.to_string())
    };

    // 1) 顶层 package.json
    if let Some(v) = read_version(&dir.join("package.json")) {
        return Some(v);
    }
    // 2) npm 版：node_modules/@deepseek-ai/dsh/package.json
    read_version(
        &dir.join("node_modules")
            .join(crate::dsh_npm::DSH_NPM_PACKAGE)
            .join("package.json"),
    )
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
pub fn semver_compare_public(a: &str, b: &str) -> std::cmp::Ordering {
    match (semver::Version::parse(a), semver::Version::parse(b)) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        _ => a.cmp(b),
    }
}

/// 简单 semver 比较（内部别名，保持旧调用不变）。
fn semver_compare(a: &str, b: &str) -> std::cmp::Ordering {
    semver_compare_public(a, b)
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

/// npm registry 上的 @deepseek-ai/dsh 版本列表（含 dist-tag / 预发布标记）。
/// 网络失败返回 Err——调用方降级为仅本地列表。
pub async fn fetch_remote_releases() -> Result<Vec<serde_json::Value>, String> {
    let versions = crate::dsh_npm::fetch_remote_versions().await?;
    // 兼容旧字段名（tag=版本号，version=版本号，prerelease，assetUrl 弃用）
    let out: Vec<serde_json::Value> = versions
        .into_iter()
        .map(|mut v| {
            let ver = v["version"].as_str().unwrap_or("").to_string();
            v["tag"] = serde_json::Value::String(ver.clone());
            v["distTag"] = v["distTag"].clone();
            v
        })
        .collect();
    log::info!("dsh 版本列表（npm）：拉到 {} 个版本", out.len());
    Ok(out)
}

/// 安装指定版本到 `dsh-versions/<tag>`（不切换激活）。
/// 方式：pnpm 从 npm registry（npmmirror）安装 @deepseek-ai/dsh@<version>。
/// tag 即 npm 版本号（如 `0.1.1-rc.2`），版本目录名用 `v<version>` 前缀区分。
pub async fn install_version<R: Runtime>(
    app: &AppHandle<R>,
    tag: &str,
    on_progress: crate::download::ProgressCallback<'_>,
) -> Result<(), String> {
    // tag 兼容：v0.1.1-rc.2 → 0.1.1-rc.2（npm 版本号不带 v）
    let version = tag.trim_start_matches('v');
    let dir_name = format!("v{version}");
    let dest = version_dir(app, &dir_name);
    if dest.join("package.json").exists() {
        log::info!("dsh 版本 {version} 已存在，跳过安装");
        return Ok(());
    }

    // 首次启用版本管理：把当前激活 dsh 备份进版本目录，避免旧版本"消失"
    ensure_active_backed_up(app);

    log::info!("pnpm 安装 dsh {version} 到版本目录…");
    crate::dsh_npm::install_to(app, &dest, version, on_progress).await?;
    let installed = version_from_dir(&dest).unwrap_or_else(|| version.to_string());
    log::info!("dsh 版本 {version} 安装完成（实际 {installed}）");
    Ok(())
}

/// 切换激活版本：停 Harness → 复制版本目录到 `dependencies/dsh` → 重启。
/// 返回 (旧版本号, 新版本号)。
pub async fn switch_version<R: Runtime>(
    app: &AppHandle<R>,
    tag: &str,
) -> Result<(String, String), String> {
    // tag 可能是 v<version>（目录名）或裸版本号 → 统一找目录
    let dir_name = if tag.starts_with('v') { tag.to_string() } else { format!("v{tag}") };
    let src = version_dir(app, &dir_name);
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

    // 2. 链接版本目录 → 激活目录（原子替换）
    // 删除可能因 Harness 进程占用而失败（Windows 文件锁）——重试等待，
    // 进程退出后锁释放（真实场景 workflow::stop 已先杀进程树）。
    let old_version = active_version(app);
    let dest = active_dir(app);
    let mut cleaned = false;
    for attempt in 1..=5 {
        match safe_remove_dir(&dest) {
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
    // 用目录链接（Windows junction / Unix symlink）替代整目录复制——
    // 复制 121MB/1 万文件需数分钟，链接瞬间完成。
    // junction 对应用透明（dsh 通过链接路径访问 node_modules 无感知）。
    #[cfg(windows)]
    {
        // cmd mklink /J <link> <target>（junction 无需管理员权限）
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&dest)
            .arg(&src)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| format!("DSH_JUNCTION_SPAWN_FAILED: {e}"))?;
        if !status.success() {
            // junction 失败（罕见）→ 回退复制
            log::warn!("创建 junction 失败，回退复制（较慢）");
            copy_dir_all(&src, &dest).map_err(|e| format!("DSH_SWAP_COPY_FAILED: {e}"))?;
        }
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(&src, &dest)
            .map_err(|e| format!("DSH_SWAP_SYMLINK_FAILED: {e}"))?;
    }
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

/// 从 release tag 提取干净版本号。
/// tag 格式多样：`v0.1.1-rc.1` / `dsh-src-0.1.2-alpha.1-33297864233` / `0.1.2`。
/// 规则：找第一个 `数字.数字.数字` 起点，取完整 semver 主干；
/// 预发布末尾的纯数字长段视为构建号（如 `-33297864233`）并去掉。
pub fn normalize_tag_version(tag: &str) -> String {
    // 找第一个数字开头的位置
    let bytes = tag.as_bytes();
    let mut start = None;
    for (i, b) in bytes.iter().enumerate() {
        if b.is_ascii_digit() {
            start = Some(i);
            break;
        }
    }
    let Some(start) = start else {
        return tag.to_string();
    };
    let rest = &tag[start..];

    // 逐字符扩展，找最长的可被 semver 解析的前缀
    let mut best = String::new();
    let mut candidate = String::new();
    for c in rest.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '+' {
            candidate.push(c);
            if semver::Version::parse(&candidate).is_ok() {
                best = candidate.clone();
            }
        } else {
            break;
        }
    }
    if best.is_empty() {
        return rest.to_string();
    }
    // 去掉构建号（+ 之后）
    if let Some(plus) = best.find('+') {
        best.truncate(plus);
    }
    // 预发布里若含「-<长数字>」（构建号特征，如 `alpha.1-33297864233`）→ 从该 - 截断
    if let Ok(ver) = semver::Version::parse(&best) {
        let pre = ver.pre;
        if !pre.is_empty() {
            let pre_str = pre.as_str();
            // 形如 alpha.1-33297864233：最后一段含连字符 + 长数字 → 截到连字符前
            if let Some(hyphen) = pre_str.rfind('-') {
                let suffix = &pre_str[hyphen + 1..];
                if suffix.len() >= 5 && suffix.chars().all(|c| c.is_ascii_digit()) {
                    let base = format!("{}.{}.{}", ver.major, ver.minor, ver.patch);
                    let trimmed_pre = &pre_str[..hyphen];
                    if trimmed_pre.is_empty() {
                        return base;
                    }
                    return format!("{base}-{trimmed_pre}");
                }
            }
        }
    }
    best
}

/// 最近一次「检查更新」拉到的远程版本列表（进程内缓存，托盘菜单渲染用）。
static REMOTE_CACHE: std::sync::Mutex<Option<Vec<serde_json::Value>>> = std::sync::Mutex::new(None);

/// 缓存远程版本列表（检查更新成功后写入）。
pub fn cache_remote_releases(list: Vec<serde_json::Value>) {
    *REMOTE_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some(list);
}

/// 读取缓存的远程版本列表（未检查过返回空）。
pub fn cached_remote_releases() -> Vec<serde_json::Value> {
    REMOTE_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

/// 远程可安装版本列表（过滤掉已装的；菜单渲染与点击安装共用，保证索引一致）。
pub fn installable_remote_releases<R: Runtime>(app: &AppHandle<R>) -> Vec<serde_json::Value> {
    let installed = list_installed(app);
    let installed_tags: std::collections::HashSet<String> = installed
        .iter()
        .filter_map(|v| v["tag"].as_str().map(|s| s.to_string()))
        .collect();
    cached_remote_releases()
        .into_iter()
        .filter(|r| {
            let tag = r["tag"].as_str().unwrap_or("");
            let version = r["version"].as_str().unwrap_or("");
            !installed_tags.contains(tag) && !installed_tags.iter().any(|t| t.contains(version))
        })
        .collect()
}

/// 检查更新：当前激活版本 vs 远程最新 release。
/// 返回 (当前版本, 最新版本, 是否有更新)。
pub async fn check_update<R: Runtime>(app: &AppHandle<R>) -> (String, Option<String>, bool) {
    let current = active_version(app);
    let remote = match fetch_remote_releases().await {
        Ok(list) => {
            // 缓存整个列表（托盘菜单显示可安装的远程版本）
            cache_remote_releases(list.clone());
            list.first().and_then(|r| r["tag"].as_str().map(|s| s.to_string()))
        }
        Err(e) => {
            log::warn!("检查 dsh 更新失败：{e}");
            None
        }
    };
    let has_update = match (&remote, current.is_empty()) {
        (Some(remote_tag), false) => {
            let remote_ver = normalize_tag_version(remote_tag);
            semver_compare(&remote_ver, &current) == std::cmp::Ordering::Greater
        }
        (Some(_), true) => true, // 未安装 → 有更新
        _ => false,
    };
    (current, remote, has_update)
}

/// 复制目录（递归，覆盖已有）。
/// 安全删除目录：junction/symlink 只删链接本身（不递归进目标），普通目录才递归删。
/// 防止切换时 `remove_dir_all` 误删版本目录内容。
fn safe_remove_dir(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            // FILE_ATTRIBUTE_REPARSE_POINT = 0x400
            if meta.file_attributes() & 0x400 != 0 {
                return std::fs::remove_dir(path).map_err(|e| format!("{e}"));
            }
        }
    }
    std::fs::remove_dir_all(path).map_err(|e| format!("{e}"))
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
    fn normalize_tag_versions() {
        // 标准 v 前缀
        assert_eq!(normalize_tag_version("v0.1.1-rc.1"), "0.1.1-rc.1");
        // 用户实测的 tag：dsh-src- 前缀 + 构建号
        assert_eq!(normalize_tag_version("dsh-src-0.1.2-alpha.1-33297864233"), "0.1.2-alpha.1");
        // 无前缀
        assert_eq!(normalize_tag_version("0.1.2"), "0.1.2");
        // 预发布多段
        assert_eq!(normalize_tag_version("v1.0.0-beta.2"), "1.0.0-beta.2");
        // 无版本号 → 原样返回
        assert_eq!(normalize_tag_version("release"), "release");
        // 空串
        assert_eq!(normalize_tag_version(""), "");
    }

    #[test]
    fn normalize_compare_detects_update() {
        // 远程 dsh-src-0.1.2-alpha.1-xxx vs 本地 0.1.1-rc.1 → 有更新
        let remote = normalize_tag_version("dsh-src-0.1.2-alpha.1-33297864233");
        assert_eq!(semver_compare(&remote, "0.1.1-rc.1"), std::cmp::Ordering::Greater);
        // 同版本无更新
        let remote2 = normalize_tag_version("v0.1.1-rc.1");
        assert_eq!(semver_compare(&remote2, "0.1.1-rc.1"), std::cmp::Ordering::Equal);
    }
}
