//! 把所选加速源写入 Harness profile 的 `.npmrc`，使插件拉包走加速/内网源。
//!
//! 幂等合并：仅追加 `confirmModulesPurge=false` 与（非默认源时）`registry=<url>`，
//! 保留用户已有配置。位于 `<dsh_home>/profiles/web/.npmrc`。

use std::path::PathBuf;
use tauri::{AppHandle, Runtime};

use crate::config::*;

const NPMRC_KEY: &str = "confirmModulesPurge=false";
const NPMRC_REGISTRY_PREFIX: &str = "registry=";
const DEFAULT_REGISTRY: &str = "https://registry.npmjs.org/";

/// 在给定 `.npmrc` 路径上幂等合并写入目标键。
fn ensure_npmrc_at(npmrc_path: &PathBuf, registry: &str) -> Result<(), String> {
    let registry = registry.trim();
    let needs_registry = !registry.is_empty() && registry != DEFAULT_REGISTRY;

    let existing = match std::fs::read_to_string(npmrc_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("NPMRC_READ_FAILED: {e}")),
    };

    let mut additions: Vec<String> = Vec::new();
    if !existing.lines().any(|l| l.trim() == NPMRC_KEY) {
        additions.push(NPMRC_KEY.to_string());
    }
    if needs_registry {
        let target = format!("{NPMRC_REGISTRY_PREFIX}{registry}");
        if !existing.lines().any(|l| l.trim() == target) {
            additions.push(target);
        }
    }

    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    for add in &additions {
        content.push_str(add);
        content.push('\n');
    }

    if let Some(dir) = npmrc_path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("NPMRC_DIR_CREATE_FAILED: {e}"))?;
    }
    std::fs::write(npmrc_path, content).map_err(|e| format!("NPMRC_WRITE_FAILED: {e}"))?;
    log::info!("已确保 profile .npmrc：{}", npmrc_path.display());
    Ok(())
}

/// 写入指定 profile 的 `.npmrc`（使用当前解析出的 npm registry）。
pub fn ensure_profile_npmrc<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> Result<(), String> {
    ensure_profile_npmrc_for(app, cfg, "web")
}

/// 写入指定 profile 的 `.npmrc`（使用当前解析出的 npm registry）。
pub fn ensure_profile_npmrc_for<R: Runtime>(
    app: &AppHandle<R>,
    cfg: &LauncherConfig,
    profile: &str,
) -> Result<(), String> {
    let registry = resolve_npm_registry(cfg);
    let npmrc = dsh_home(app, cfg)
        .join("profiles")
        .join(profile)
        .join(".npmrc");
    ensure_npmrc_at(&npmrc, &registry)
}
