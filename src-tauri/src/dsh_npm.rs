//! dsh 的 npm 安装方案：从 npm registry 安装 @deepseek-ai/dsh，替代 GitHub release zip。
//!
//! 背景：GitHub release 下载在国内不稳定，而 @deepseek-ai/dsh 在 npm 有完整发布
//! （npmmirror 镜像稳定），版本精确可控（dist-tags: latest/alpha/next）。
//!
//! 安装 = 在目标目录建 package.json（声明 @deepseek-ai/dsh 依赖 + pnpm.onlyBuiltDependencies）
//! → pnpm install/add → 产出 node_modules/@deepseek-ai/dsh（bin.js 可执行）。
//!
//! 目录结构与旧 GitHub 版（deepseek-harness-pkg 薄壳）兼容：都是 node_modules 顶层，
//! dsh_binary_path = <dest>/node_modules/@deepseek-ai/dsh/lib/bin.js 不变。

use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tauri::{AppHandle, Runtime};

use crate::config::*;

/// npm 上的 dsh 包名。
pub const DSH_NPM_PACKAGE: &str = "@deepseek-ai/dsh";

/// 需要允许 build 脚本的原生依赖（pnpm 默认阻止 build；这些是 dsh 运行关键）。
const ALLOWED_BUILDS: &[&str] = &[
    "@deepseek-ai/dsh-subprocess-local",
    "koffi",
    "node-pty",
    "protobufjs",
    "@google/genai",
];

// ---------- npm registry 查询 ----------

/// 查询 @deepseek-ai/dsh 的 dist-tags 与版本列表（按序尝试 npmmirror → npmjs）。
/// 返回 { distTags: {...}, versions: [...] }。
pub async fn fetch_npm_meta() -> Result<serde_json::Value, String> {
    let client = reqwest::Client::builder()
        .user_agent("dsh-harness-launcher-npm")
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;

    let encoded = DSH_NPM_PACKAGE.replace('/', "%2F");
    let mut last_err = String::new();
    // registry 候选：显式配置的内网 registry → npmmirror → npmjs
    let mut registries: Vec<String> = Vec::new();
    let cfg = load_cached();
    if let Some(ms) = &cfg.mirror_settings {
        if let Some(reg) = &ms.registry {
            if !reg.is_empty() {
                registries.push(reg.trim_end_matches('/').to_string());
            }
        }
    }
    registries.push("https://registry.npmmirror.com".to_string());
    registries.push("https://registry.npmjs.org".to_string());
    for reg in registries {
        let url = format!("{reg}/{encoded}");
        match client.get(&url).send().await {
            Ok(res) if res.status().is_success() => {
                let meta: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
                let dist_tags = meta.get("dist-tags").cloned().unwrap_or_else(|| serde_json::json!({}));
                let versions: Vec<String> = meta
                    .get("versions")
                    .and_then(|v| v.as_object())
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default();
                log::info!(
                    "dsh npm 元信息：源={reg} dist-tags={} 版本数={}",
                    dist_tags,
                    versions.len()
                );
                return Ok(serde_json::json!({
                    "distTags": dist_tags,
                    "versions": versions,
                }));
            }
            Ok(res) => last_err = format!("HTTP {}", res.status()),
            Err(e) => last_err = e.to_string(),
        }
    }
    Err(format!("DSH_NPM_FETCH_FAILED: {last_err}"))
}

/// 最新稳定版本（dist-tags.latest）。失败返回空串（调用方降级）。
pub async fn latest_version() -> Result<String, String> {
    let meta = fetch_npm_meta().await?;
    meta.get("distTags")
        .and_then(|d| d.get("latest"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "DSH_NPM_NO_LATEST: dist-tags.latest 缺失".to_string())
}

/// 远程版本列表（用于「检查更新 / 安装指定版本」）。
/// 返回每项 { version, prerelease, distTag }；按版本号倒序（最新在前）。
pub async fn fetch_remote_versions() -> Result<Vec<serde_json::Value>, String> {
    let meta = fetch_npm_meta().await?;
    let dist_tags = meta.get("distTags").cloned().unwrap_or_default();
    let versions: Vec<String> = meta
        .get("versions")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    // 标记 dist-tag（latest/alpha/next）
    let mut out: Vec<serde_json::Value> = versions
        .iter()
        .map(|v| {
            let tag = dist_tags
                .as_object()
                .and_then(|m| m.iter().find(|(_, val)| val.as_str() == Some(v.as_str())))
                .map(|(k, _)| k.clone());
            serde_json::json!({
                "version": v,
                "prerelease": v.contains('-'),
                "distTag": tag.unwrap_or_default(),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        let va = a["version"].as_str().unwrap_or("");
        let vb = b["version"].as_str().unwrap_or("");
        crate::dsh_versions::semver_compare_public(vb, va)
    });
    Ok(out)
}

// ---------- 安装 ----------

/// 在目标目录安装 @deepseek-ai/dsh@<version>（pnpm add）。
/// 目标目录被完整重建（package.json + node_modules）。
pub async fn install_to<R: Runtime>(
    app: &AppHandle<R>,
    dest: &Path,
    version: &str,
    on_progress: crate::download::ProgressCallback<'_>,
) -> Result<(), String> {
    let cfg = load_cached();
    let node = effective_node_path(app, &cfg);
    let pnpm = pnpm_binary_path(app);
    if !node.exists() || !pnpm.exists() {
        return Err("PNPM_OR_NODE_NOT_FOUND: 请先安装 Node.js / pnpm".to_string());
    }

    // 重建目标目录（原子：先装 staging，成功后再替换）
    let parent = dest.parent().unwrap_or(Path::new(".")).to_path_buf();
    let leaf = dest.file_name().and_then(|v| v.to_str()).unwrap_or("dsh").to_string();
    let staging = parent.join(format!(".{leaf}.installing-{}", std::process::id()));
    // 清理历史残留的 installing 目录（上次失败/中断留下的）
    if let Ok(entries) = std::fs::read_dir(&parent) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&format!(".{leaf}.installing-")) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    let _ = remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;

    // 写 package.json：依赖 @deepseek-ai/dsh 指定版本
    let manifest = serde_json::json!({
        "name": "dsh-runtime",
        "private": true,
        "dependencies": {
            DSH_NPM_PACKAGE: version,
        },
    });
    std::fs::write(
        staging.join("package.json"),
        serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    // pnpm 11 不再读 package.json 的 pnpm 字段——原生依赖 build 白名单
    // 写在 pnpm-workspace.yaml 的 allowBuilds（pnpm 11 替代 onlyBuiltDependencies）。
    // scoped 包名（含 @ /）必须加引号，否则 YAML 解析失败。
    let workspace_yaml = format!(
        "packages:\n  - .\nallowBuilds:\n{}",
        ALLOWED_BUILDS
            .iter()
            .map(|p| format!("  \"{p}\": true"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    std::fs::write(staging.join("pnpm-workspace.yaml"), workspace_yaml)
        .map_err(|e| e.to_string())?;

    log::info!("pnpm 安装 {}@{} 到 {}…", DSH_NPM_PACKAGE, version, staging.display());
    let result = run_pnpm(&node, &pnpm, &staging, &["install"], on_progress).await?;
    log::info!("pnpm install 输出：{}", result.trim().lines().last().unwrap_or(""));

    // 验证 bin.js
    let bin = staging
        .join("node_modules")
        .join(DSH_NPM_PACKAGE)
        .join("lib/bin.js");
    if !bin.exists() {
        let _ = remove_dir_all(&staging);
        return Err(format!("DSH_NPM_BIN_MISSING: 安装后未找到 {bin:?}"));
    }

    // 原子替换目标目录
    if dest.exists() {
        remove_dir_all(dest).map_err(|e| format!("DSH_DEST_CLEAN_FAILED: {e}"))?;
    }
    std::fs::rename(&staging, dest).map_err(|e| format!("DSH_DEST_COMMIT_FAILED: {e}"))?;
    log::info!("dsh {} 安装完成：{}", version, dest.display());
    Ok(())
}

/// 执行 pnpm 命令（node <pnpm.cjs> install），带超时。
async fn run_pnpm(
    node: &Path,
    pnpm: &Path,
    cwd: &Path,
    args: &[&str],
    _on_progress: crate::download::ProgressCallback<'_>,
) -> Result<String, String> {
    // pnpm install 输出量大，进度回调不逐行（下载进度由 pnpm 自身打印）
    let mut cmd = Command::new(node);
    cmd.arg(pnpm)
        .args(args)
        .current_dir(cwd)
        .env("npm_config_registry", npm_registry_for_install())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // 前置 node 目录到 PATH（pnpm 内部调 node）
    if let Some(existing) = std::env::var_os("PATH") {
        let mut paths = std::env::split_paths(&existing).collect::<Vec<_>>();
        if let Some(dir) = node.parent() {
            if !paths.contains(&dir.to_path_buf()) {
                paths.insert(0, dir.to_path_buf());
            }
        }
        if let Ok(joined) = std::env::join_paths(paths) {
            cmd.env("PATH", joined);
        }
    }

    let child = cmd.spawn().map_err(|e| format!("pnpm spawn: {e}"))?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    let t = std::thread::spawn(move || {
        let out = child.wait_with_output();
        let _ = tx.send(out);
    });
    // pnpm 安装可能较久（几百个包），给 10 分钟
    const PNPM_TIMEOUT: Duration = Duration::from_secs(600);
    match rx.recv_timeout(PNPM_TIMEOUT) {
        Ok(output) => {
            let output = output.map_err(|e| format!("pnpm wait: {e}"))?;
            let _ = t.join();
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                log::error!("pnpm install 失败：{}", stderr.trim());
                // 拼 stdout 尾部（含错误上下文）
                let tail = stdout.trim().lines().rev().take(10).collect::<Vec<_>>().join("\n");
                return Err(format!("PNPM_INSTALL_FAILED: {}", if tail.is_empty() { stderr.trim().to_string() } else { tail }));
            }
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        }
        Err(_) => {
            let _ = t.join();
            kill_tree(pid);
            Err(format!("PNPM_TIMEOUT: 安装超过 {}s，已终止", PNPM_TIMEOUT.as_secs()))
        }
    }
}

/// pnpm 安装用的 registry：
/// 1) 显式配置的内网 registry（mirrorSettings.registry——管理员统一镜像了 npm 包）
/// 2) 按地域：国内 npmmirror（稳定）、海外 npmjs
fn npm_registry_for_install() -> String {
    let cfg = load_cached();
    // 显式配置了内网 registry 才用（默认值 registry.ict.cmcc 是兜底，不代表真实内网）
    if let Some(ms) = &cfg.mirror_settings {
        if let Some(reg) = &ms.registry {
            if !reg.is_empty() {
                return reg.clone();
            }
        }
    }
    match crate::config::detect_region() {
        crate::config::Region::Domestic => "https://registry.npmmirror.com".to_string(),
        crate::config::Region::Overseas => "https://registry.npmjs.org".to_string(),
    }
}

/// 判断目录是否已是 npm 结构（有 node_modules/@deepseek-ai/dsh）。
pub fn is_npm_installed(dest: &Path) -> bool {
    dest.join("node_modules")
        .join(DSH_NPM_PACKAGE)
        .join("lib/bin.js")
        .exists()
}

/// 安全删除目录：junction/symlink 只删链接本身（不递归进目标），普通目录才递归删。
/// 防止 `remove_dir_all` 对 junction 误删版本目录内容。
fn remove_dir_all(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // reparse point（junction/symlink）→ 只删链接
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            if meta.file_attributes() & 0x400 != 0 {
                return std::fs::remove_dir(path).map_err(|e| format!("{e}"));
            }
        }
    }
    std::fs::remove_dir_all(path).map_err(|e| format!("{e}"))
}

fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_name_constant() {
        assert_eq!(DSH_NPM_PACKAGE, "@deepseek-ai/dsh");
        assert!(ALLOWED_BUILDS.contains(&"koffi"));
        assert!(ALLOWED_BUILDS.contains(&"node-pty"));
    }

    #[test]
    fn npm_manifest_structure() {
        let manifest = serde_json::json!({
            "name": "dsh-runtime",
            "private": true,
            "dependencies": { DSH_NPM_PACKAGE: "0.1.1-rc.2" },
        });
        assert_eq!(manifest["dependencies"][DSH_NPM_PACKAGE], "0.1.1-rc.2");
    }

    #[test]
    fn workspace_yaml_lists_allowed_builds() {
        // pnpm 11 用 pnpm-workspace.yaml 的 allowBuilds（替代 onlyBuiltDependencies）
        // scoped 包名必须加引号
        let yaml = format!(
            "packages:\n  - .\nallowBuilds:\n{}",
            ALLOWED_BUILDS
                .iter()
                .map(|p| format!("  \"{p}\": true"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        assert!(yaml.contains("allowBuilds:"));
        assert!(yaml.contains("\"koffi\": true"));
        assert!(yaml.contains("\"node-pty\": true"));
        assert!(yaml.contains("\"@deepseek-ai/dsh-subprocess-local\": true"));
    }
}
