//! IPC 命令：常驻实例通过 `invoke` 调用（前端/测试脚本），
//! 与 CLI 命令共享执行核心，返回统一 JSON（{ok, ...}）。
//!
//! 测试价值：IPC 让测试脚本针对**运行中的实例**验证常驻行为
//! （托盘状态、进度窗口、同步循环等），而 CLI 验证一次性操作。

use serde_json::json;
use tauri::{AppHandle, Runtime};

/// 统一响应：ok + 可选字段。
fn ok(v: serde_json::Value) -> serde_json::Value {
    let mut o = json!({"ok": true});
    if let Some(obj) = v.as_object() {
        for (k, val) in obj {
            o[k] = val.clone();
        }
    }
    o
}

/// 安装/修复全部依赖。
#[tauri::command]
pub async fn cmd_install<R: Runtime>(app: AppHandle<R>) -> Result<serde_json::Value, String> {
    crate::install::install_all(&app).await.map_err(|e| e)?;
    Ok(ok(json!({"message": "安装完成"})))
}

/// 启动 Harness。
#[tauri::command]
pub fn cmd_launch<R: Runtime>(app: AppHandle<R>) -> Result<serde_json::Value, String> {
    match crate::workflow::launch(&app) {
        Ok(port) => Ok(ok(json!({"port": port}))),
        Err(e) => Err(e),
    }
}

/// 停止 Harness。
#[tauri::command]
pub fn cmd_stop<R: Runtime>(app: AppHandle<R>) -> Result<serde_json::Value, String> {
    crate::workflow::stop();
    let _ = app;
    Ok(ok(json!({})))
}

/// 立即同步。
#[tauri::command]
pub async fn cmd_sync<R: Runtime>(app: AppHandle<R>) -> Result<serde_json::Value, String> {
    let cfg = crate::config::load_cached();
    let outcome = crate::sync::sync_once(&app, &cfg, None).await;
    match outcome.config {
        Some(_) => Ok(ok(json!({"pending": outcome.pending}))),
        None => Err("无法连接中心服务端（已使用本地缓存）".to_string()),
    }
}

/// 测速（结果缓存到进程内，供托盘菜单显示）。
#[tauri::command]
pub async fn cmd_speedtest<R: Runtime>(app: AppHandle<R>) -> Result<serde_json::Value, String> {
    let cfg = crate::config::load_cached();
    let npm = crate::speedtest::speedtest_npm(&cfg).await;
    let gh = crate::speedtest::speedtest_gh(&cfg).await;
    let mut all = npm.clone();
    all.extend(gh.clone());
    crate::speedtest::set_last_results(all);
    let results: Vec<serde_json::Value> = npm
        .iter()
        .chain(gh.iter())
        .map(|r| {
            json!({"name": r.name, "url": r.url, "latency_ms": r.latency_ms, "ok": r.ok})
        })
        .collect();
    let _ = app;
    Ok(ok(json!({"results": results})))
}

/// 镜像上传（registry + token 经参数传入，token 不落盘）。
#[tauri::command]
pub fn cmd_mirror<R: Runtime>(
    app: AppHandle<R>,
    registry: Option<String>,
    token: String,
) -> Result<serde_json::Value, String> {
    let cfg = crate::config::load_cached();
    let reg = registry
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| crate::config::mirror_registry(&cfg));
    match crate::mirror::start_mirror_upload(&app, &cfg, &reg, &token, None) {
        Ok(()) => Ok(ok(json!({"registry": reg}))),
        Err(e) => Err(e),
    }
}

/// 查询状态（运行/端口/profile/上次操作/测速结果）。
#[tauri::command]
pub fn cmd_status<R: Runtime>(app: AppHandle<R>) -> Result<serde_json::Value, String> {
    let running = crate::workflow::is_running();
    let port = crate::workflow::last_port();
    let profile = crate::workflow::current_profile();
    let op = crate::ops::current();
    let results = crate::speedtest::last_results();
    let op_json = op.map(|o| {
        serde_json::to_value(&o).unwrap_or_else(|_| json!(null))
    });
    let results_json: Vec<serde_json::Value> = results
        .iter()
        .map(|r| json!({"name": r.name, "latency_ms": r.latency_ms, "ok": r.ok}))
        .collect();
    let _ = app;
    Ok(ok(json!({
        "running": running,
        "port": port,
        "profile": profile,
        "op": op_json,
        "speedtest": results_json,
    })))
}

/// 打开进度窗口。
#[tauri::command]
pub fn cmd_open_console<R: Runtime>(app: AppHandle<R>) -> Result<serde_json::Value, String> {
    crate::console::open_console(&app).map_err(|e| e)?;
    Ok(ok(json!({})))
}

/// 查询 dsh 版本状态（当前激活 / 已装列表 / 远程最新 / 是否有更新）。
#[tauri::command]
pub async fn cmd_dsh_versions<R: Runtime>(app: AppHandle<R>) -> Result<serde_json::Value, String> {
    let installed = crate::dsh_versions::list_installed(&app);
    let active = crate::dsh_versions::active_version(&app);
    let active_tag = crate::dsh_versions::active_tag(&app);
    // 标记激活项
    let installed: Vec<serde_json::Value> = installed
        .into_iter()
        .map(|mut v| {
            if v["tag"].as_str() == Some(active_tag.as_str()) {
                v["active"] = serde_json::Value::Bool(true);
            }
            v
        })
        .collect();
    // 远程最新（失败返回 null，不阻断）
    let (current, latest, has_update) = crate::dsh_versions::check_update(&app).await;
    Ok(ok(json!({
        "active": active,
        "activeTag": active_tag,
        "current": current,
        "installed": installed,
        "latest": latest,
        "hasUpdate": has_update,
    })))
}

/// 下载并安装指定 dsh 版本（不切换）。
#[tauri::command]
pub async fn cmd_dsh_install<R: Runtime>(
    app: AppHandle<R>,
    tag: String,
) -> Result<serde_json::Value, String> {
    crate::dsh_versions::install_version(&app, &tag, None)
        .await
        .map_err(|e| e)?;
    Ok(ok(json!({"tag": tag, "message": "已安装"})))
}

/// 切换 dsh 版本（停 Harness → 换版本 → 重启）。
#[tauri::command]
pub async fn cmd_dsh_switch<R: Runtime>(
    app: AppHandle<R>,
    tag: String,
) -> Result<serde_json::Value, String> {
    let (old, new) = crate::dsh_versions::switch_version(&app, &tag)
        .await
        .map_err(|e| e)?;
    Ok(ok(json!({"old": old, "new": new, "message": format!("{old} -> {new}")})))
}
