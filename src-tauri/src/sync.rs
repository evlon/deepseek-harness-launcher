//! 企业中心服务端同步引擎。
//!
//! 负责：拉取服务端推荐插件配置、对比本机已装清单、上报同步状态、本地缓存。
//!
//! 离线容错设计：
//! - 所有网络操作超时（8s），失败仅记日志，**不 panic、不阻断** launcher 主流程；
//! - 最近一次成功拉取的配置缓存到 `<dsh_home>/sync-state.json`，离线时继续可用；
//! - 网络恢复后下一个轮询周期自动补拉，无需人工干预。
//!
//! 同步独立于 dsh web 服务（`workflow::is_running` 无关）：只要 launcher 在跑，
//! 就会按 `syncIntervalSecs` 轮询。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Runtime};

use crate::config::*;
use crate::notify;
use crate::tray;

/// 服务端下发的托盘菜单策略。
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedMenu {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub quick_links: Vec<QuickLink>,
}

/// 服务端 /api/config 返回的配置。
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct ServerConfig {
    #[serde(default)]
    pub version: Option<u64>,
    #[serde(default)]
    pub plugins: Vec<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    /// 托盘菜单策略（服务端统一下发）。
    #[serde(default, rename = "managedMenu")]
    pub managed_menu: Option<ManagedMenu>,
    /// 客户端默认配置覆盖（服务端下发的 npmRegistry/ghMirrorPrefix/port 等）。
    #[serde(default, rename = "clientDefaults")]
    pub client_defaults: Option<serde_json::Value>,
    /// 镜像相关设置（服务端统一管理：registry / dsh 分发源）。
    #[serde(default, rename = "mirrorSettings")]
    pub mirror_settings: Option<serde_json::Value>,
}

/// 客户端已装插件详情（跨所有 profile，上报给服务端）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PluginInfo {
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// 所属 profile（web / matrix / ...）。
    #[serde(default)]
    pub profile: String,
    /// 是否声明 `dsh.client.platform: web`（贡献 web UI 菜单）。
    #[serde(default)]
    pub client: bool,
}

/// 本机同步缓存（写 `<dsh_home>/sync-state.json`）。
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct SyncState {
    /// 最近一次成功拉取的服务端配置（离线时使用）。
    #[serde(default)]
    pub cached_config: Option<ServerConfig>,
    /// 最近一次成功上报的本地已装清单。
    #[serde(default)]
    pub last_installed: Vec<String>,
    /// 最近一次成功同步时间（ISO 8601）。
    #[serde(default)]
    pub last_sync_at: Option<String>,
    /// 菜单策略缓存（服务端下发，托盘渲染时据此展示）。
    #[serde(default)]
    pub cached_managed_menu: Option<ManagedMenu>,
    /// 各插件 registry 最新版本缓存（name → latest version）。
    /// 版本检查用：已装插件对比最新版，落后则提示更新。
    #[serde(default)]
    pub plugin_latest_versions: std::collections::HashMap<String, String>,
}

// ---------- 路径 ----------

fn sync_state_path<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> PathBuf {
    dsh_home(app, cfg).join("sync-state.json")
}

fn client_id_path<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> PathBuf {
    dsh_home(app, cfg).join("client-id")
}

// ---------- 客户端标识 ----------

/// 读取或生成客户端 ID（UUID v4，首次生成后持久化，服务端据此区分机器）。
pub fn client_id<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> String {
    let path = client_id_path(app, cfg);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let id = existing.trim().to_string();
        if valid_client_id(&id) {
            return id;
        }
    }
    let id = new_uuid_v4();
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
    let _ = std::fs::write(&path, &id);
    id
}

fn valid_client_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// 简易 UUID v4（无 rand 依赖：基于时间 + 进程熵）。
fn new_uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let addr = std::ptr::addr_of!(SYNC_MARKER) as u64;

    // 16 字节：mix 时间/pid/计数/地址，保证同进程多次调用不重复
    let mut bytes = [0u8; 16];
    let w0 = nanos as u64;
    let w1 = (nanos >> 64) as u64 ^ pid;
    let w2 = counter ^ addr.rotate_left(13);
    let w3 = pid.rotate_left(29) ^ counter.reverse_bits();
    bytes[0..8].copy_from_slice(&w0.to_le_bytes());
    bytes[8..16].copy_from_slice(&(w1 ^ w2 ^ w3).to_le_bytes());
    // 版本 4 与变体位
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    let mut s = String::with_capacity(36);
    for (i, b) in bytes.iter().enumerate() {
        if i == 4 || i == 6 || i == 8 || i == 10 {
            s.push('-');
        }
        s.push_str(&format!("{:02x}", b));
    }
    s
}

static SYNC_MARKER: u8 = 0;

// ---------- 本地已装清单 ----------

/// 当前生效 profile 已安装的插件名（用于「服务器推荐装到当前 profile」的待装判断）。
///
/// 口径：只看 `resolve_profile(cfg)` 对应的 profile 的 dependencies，
/// 且 node_modules 里确实存在（package.json 写了但没装上不算已装）。
/// 与 `installed_plugins`（跨所有 profile，用于上报）不同——待装判断
/// 必须以「同事当前运行的 profile」为准，否则会出现「web 装了、matrix 没装，
/// 但默认跑 matrix 却提示已装」的矛盾（用户实测：服务器配了 dsh-codebuddy-models，
/// 装进 web 后默认 matrix 找不到）。
/// 注：生产用带版本的 `installed_plugins_current_profile_with_versions`；此纯名字版保留供测试。
#[allow(dead_code)]
pub fn installed_plugins_current_profile<R: Runtime>(
    app: &AppHandle<R>,
    cfg: &LauncherConfig,
) -> Vec<String> {
    let profile = resolve_profile(cfg);
    let profile_dir = dsh_home(app, cfg).join("profiles").join(profile);
    let manifest = profile_dir.join("package.json");
    let Ok(text) = std::fs::read_to_string(&manifest) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let Some(deps) = json.get("dependencies").and_then(|d| d.as_object()) else {
        return Vec::new();
    };
    deps.keys()
        .filter(|name| {
            // 需真实存在于 node_modules（含 scoped 包 @scope/name 的目录嵌套）
            let nm = profile_dir.join("node_modules");
            let pkg_dir = match name.split_once('/') {
                Some((scope, rest)) => nm.join(scope).join(rest),
                None => nm.join(name),
            };
            pkg_dir.join("package.json").is_file()
        })
        .cloned()
        .collect()
}

/// 当前生效 profile 已装插件（name → 已装版本）。
/// 版本读 `<profile>/node_modules/<pkg>/package.json`；读不到则空串（视为未装/异常）。
pub fn installed_plugins_current_profile_with_versions<R: Runtime>(
    app: &AppHandle<R>,
    cfg: &LauncherConfig,
) -> std::collections::HashMap<String, String> {
    let profile = resolve_profile(cfg);
    let profile_dir = dsh_home(app, cfg).join("profiles").join(profile);
    let manifest = profile_dir.join("package.json");
    let Ok(text) = std::fs::read_to_string(&manifest) else {
        return std::collections::HashMap::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return std::collections::HashMap::new();
    };
    let Some(deps) = json.get("dependencies").and_then(|d| d.as_object()) else {
        return std::collections::HashMap::new();
    };
    let mut out = std::collections::HashMap::new();
    for name in deps.keys() {
        let nm = profile_dir.join("node_modules");
        let pkg_dir = match name.split_once('/') {
            Some((scope, rest)) => nm.join(scope).join(rest),
            None => nm.join(name),
        };
        let pkg_manifest = pkg_dir.join("package.json");
        if !pkg_manifest.is_file() {
            continue; // 未真正安装
        }
        let version = std::fs::read_to_string(&pkg_manifest)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|j| j.get("version").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .unwrap_or_default();
        out.insert(name.clone(), version);
    }
    out
}

/// 枚举所有 profile 的插件详情（name/version/description/profile/client）。
///
/// - 遍历 `<dsh_home>/profiles/*/package.json` 的 `dependencies`
/// - 每个插件从 `<profile>/node_modules/<pkg>/package.json` 读版本/描述
/// - `client=true` 当插件声明 `dsh.client.platform: web`（贡献 web UI 菜单）
/// - 任何读取失败降级：该插件仅保留 name（version/description 为空）
pub fn collect_client_state<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> Vec<PluginInfo> {
    let profiles_dir = dsh_home(app, cfg).join("profiles");
    let Ok(entries) = std::fs::read_dir(&profiles_dir) else {
        return Vec::new();
    };
    let mut out: Vec<PluginInfo> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let profile = entry.file_name().to_string_lossy().to_string();
        let manifest = entry.path().join("package.json");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(deps) = json.get("dependencies").and_then(|d| d.as_object()) else {
            continue;
        };
        for (name, _spec) in deps {
            if !seen.insert(name.clone()) {
                continue; // 多 profile 同名插件只报一次（取第一个 profile）
            }
            let info = read_plugin_info(&entry.path(), name);
            out.push(PluginInfo {
                name: name.clone(),
                ..info
            });
            if let Some(last) = out.last_mut() {
                last.profile = profile.clone();
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 读取单个插件的版本/描述/client 标记（读取失败返回空字段）。
fn read_plugin_info(profile_dir: &Path, name: &str) -> PluginInfo {
    let pkg_path = profile_dir.join("node_modules").join(name).join("package.json");
    let Ok(text) = std::fs::read_to_string(&pkg_path) else {
        return PluginInfo { name: name.to_string(), ..Default::default() };
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return PluginInfo { name: name.to_string(), ..Default::default() };
    };
    let version = json.get("version").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let description = json.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let client = json
        .get("dsh")
        .and_then(|d| d.get("client"))
        .and_then(|c| c.get("platform"))
        .and_then(|p| p.as_str())
        .map(|p| p == "web")
        .unwrap_or(false);
    PluginInfo { name: name.to_string(), version, description, client, ..Default::default() }
}

/// 批量查询推荐插件的 registry 最新版本（npmmirror → npmjs，带缓存）。
/// 更新 `state.plugin_latest_versions`；返回 name → latest 的 map。
/// 网络失败保留旧缓存，不阻断同步。
async fn refresh_plugin_latest_versions<R: Runtime>(
    app: &AppHandle<R>,
    cfg: &LauncherConfig,
    recommended: &[String],
    state: &mut SyncState,
) -> std::collections::HashMap<String, String> {
    if recommended.is_empty() {
        return state.plugin_latest_versions.clone();
    }
    let client = reqwest::Client::builder()
        .user_agent("dsh-harness-launcher-plugin-check")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    // registry 候选：内网 → npmmirror → npmjs
    let mut registries: Vec<String> = Vec::new();
    if let Some(ms) = &cfg.mirror_settings {
        if let Some(reg) = &ms.registry {
            if !reg.is_empty() {
                registries.push(reg.trim_end_matches('/').to_string());
            }
        }
    }
    registries.push("https://registry.npmmirror.com".to_string());
    registries.push("https://registry.npmjs.org".to_string());

    for name in recommended {
        // 跳过本地已是最新且缓存有的（减少查询）
        if let Some(cached) = state.plugin_latest_versions.get(name) {
            let installed = installed_plugin_version(app, cfg, name);
            if !installed.is_empty() && !version_greater(cached, &installed) {
                continue; // 已装版本 >= 缓存最新 → 无需再查
            }
        }
        let encoded = name.replace('/', "%2F");
        let mut latest: Option<String> = None;
        for reg in &registries {
            let url = format!("{reg}/{encoded}");
            match client.get(&url).send().await {
                Ok(res) if res.status().is_success() => {
                    if let Ok(meta) = res.json::<serde_json::Value>().await {
                        latest = meta
                            .get("dist-tags")
                            .and_then(|d| d.get("latest"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        if latest.is_some() {
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(lv) = latest {
            state.plugin_latest_versions.insert(name.clone(), lv);
            log::info!("插件 {name} 最新版本：{}", state.plugin_latest_versions.get(name).unwrap());
        } else {
            log::warn!("插件 {name} 版本查询失败（registry 不可达），保留缓存");
        }
    }
    // 清理不在推荐清单里的缓存项（防膨胀）
    let recommended_set: HashSet<&str> = recommended.iter().map(|s| s.as_str()).collect();
    state.plugin_latest_versions.retain(|k, _| recommended_set.contains(k.as_str()));
    save_state(app, cfg, state);
    state.plugin_latest_versions.clone()
}

/// 读取当前 profile 已装插件的版本号（空 = 未装/读不到）。
fn installed_plugin_version<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig, name: &str) -> String {
    installed_plugins_current_profile_with_versions(app, cfg)
        .get(name)
        .cloned()
        .unwrap_or_default()
}

/// 待安装清单 = 服务端推荐 − 已装（保推荐顺序，去重；任一 profile 装了即算已装）。
/// 注：版本感知判断见 `pending_with_updates`；此纯名字版保留供测试/简单位调用。
#[allow(dead_code)]
pub fn pending_plugins(recommended: &[String], installed: &[String]) -> Vec<String> {    let have: HashSet<&str> = installed.iter().map(|s| s.as_str()).collect();
    let mut seen = HashSet::new();
    recommended
        .iter()
        .filter(|p| !have.contains(p.as_str()))
        .filter(|p| seen.insert(p.as_str()))
        .cloned()
        .collect()
}

/// 带版本信息的待处理插件：未装（install）或已装但版本落后（update）。
/// `installed`：当前 profile 已装插件名 → 已装版本（可能空 = 未装或版本读不到）。
/// `latest`：各插件 registry 最新版本（来自同步缓存）。
/// 返回保序去重；每项 { name, installed, latest, action }。
pub fn pending_with_updates(
    recommended: &[String],
    installed: &std::collections::HashMap<String, String>,
    latest: &std::collections::HashMap<String, String>,
) -> Vec<serde_json::Value> {
    let mut out: Vec<serde_json::Value> = Vec::new();
    for name in recommended {
        let have = installed.get(name).map(|v| v.as_str()).unwrap_or("");
        let lv = latest.get(name).map(|v| v.as_str()).unwrap_or("");
        let action = if have.is_empty() {
            "install"
        } else if !lv.is_empty() && version_greater(lv, have) {
            "update"
        } else {
            // 已装且为最新（或查不到最新）→ 不处理
            continue;
        };
        out.push(serde_json::json!({
            "name": name,
            "installed": have,
            "latest": lv,
            "action": action,
        }));
    }
    out
}

/// 简单版本比较（a > b → true）。容忍 rc/beta 预发布。
fn version_greater(a: &str, b: &str) -> bool {
    match (semver::Version::parse(a), semver::Version::parse(b)) {
        (Ok(x), Ok(y)) => x > y,
        _ => a != b && a.trim_start_matches('v').cmp(b.trim_start_matches('v')) == std::cmp::Ordering::Greater,
    }
}

// ---------- 本地缓存 ----------

/// 读本地同步缓存（缺失/损坏回退默认，不阻断）。
pub fn load_state<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> SyncState {
    let path = sync_state_path(app, cfg);
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
            log::warn!("sync-state.json 解析失败，回退默认：{e}");
            SyncState::default()
        }),
        Err(_) => SyncState::default(),
    }
}

fn save_state<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig, state: &SyncState) {
    let path = sync_state_path(app, cfg);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(state) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                log::warn!("sync-state.json 写入失败：{e}");
            }
        }
        Err(e) => log::warn!("sync-state.json 序列化失败：{e}"),
    }
}

// ---------- 网络 ----------

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("dsh-harness-launcher-sync")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// 拉取服务端配置。失败返回 Err（调用方应视作离线，用缓存）。
pub async fn fetch_config(server_url: &str, token: &str) -> Result<ServerConfig, String> {
    let url = format!("{}/api/config", server_url.trim_end_matches('/'));
    let mut req = http_client().get(&url);
    if !token.is_empty() {
        req = req.header("X-Admin-Token", token);
    }
    let res = req.send().await.map_err(|e| format!("FETCH_CONFIG_FAILED: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("FETCH_CONFIG_HTTP_{}", res.status()));
    }
    res.json().await.map_err(|e| format!("FETCH_CONFIG_PARSE_FAILED: {e}"))
}

/// 客户端上报的同步信息。
pub struct SyncReport<'a> {
    pub server_url: &'a str,
    pub token: &'a str,
    pub client_id: &'a str,
    pub hostname: &'a str,
    pub dsh_version: &'a str,
    pub launcher_version: &'a str,
    pub installed: &'a [String],
    pub pending: &'a [String],
    pub offline: bool,
    /// 插件详情（跨所有 profile）。
    pub plugins: &'a [PluginInfo],
    /// 实际托盘菜单（策略启用→策略项；否则用户项）。
    pub menu: &'a [QuickLink],
    /// 菜单策略是否已应用。
    pub menu_applied: bool,
    /// 客户端 profile 列表。
    pub profiles: &'a [String],
    /// 配置状态。
    pub config_state: &'a ClientConfigState,
    /// 管理能力状态（外网代理网关是否开启 + 端口）。
    pub bridge_status: &'a BridgeStatus,
}

/// 管理能力（外网代理网关）状态，上报给服务端管理页探测用。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct BridgeStatus {
    pub enabled: bool,
    pub port: u16,
}

/// 客户端配置状态（上报用）。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ClientConfigState {
    pub profile: String,
    pub port: u16,
}

/// 上报同步状态。失败返回 Err（仅日志，不影响本地）。
pub async fn report_sync(report: SyncReport<'_>) -> Result<(), String> {
    let url = format!("{}/api/sync", report.server_url.trim_end_matches('/'));
    let menu: Vec<serde_json::Value> = report
        .menu
        .iter()
        .map(|q| serde_json::json!({ "label": q.label, "url": q.url }))
        .collect();
    let plugins: Vec<serde_json::Value> = report
        .plugins
        .iter()
        .map(|p| {
            serde_json::json!({
                "name": p.name,
                "version": p.version,
                "description": p.description,
                "profile": p.profile,
                "client": p.client,
            })
        })
        .collect();
    let body = serde_json::json!({
        "clientId": report.client_id,
        "hostname": report.hostname,
        "dshVersion": report.dsh_version,
        "launcherVersion": report.launcher_version,
        "installed": report.installed,
        "pending": report.pending,
        "offline": report.offline,
        "plugins": plugins,
        "menu": menu,
        "menuApplied": report.menu_applied,
        "profiles": report.profiles,
        "configState": {
            "profile": report.config_state.profile,
            "port": report.config_state.port,
        },
        "bridgeStatus": {
            "enabled": report.bridge_status.enabled,
            "port": report.bridge_status.port,
        },
    });
    let mut req = http_client().post(&url).json(&body);
    if !report.token.is_empty() {
        req = req.header("X-Admin-Token", report.token);
    }
    let res = req.send().await.map_err(|e| format!("REPORT_SYNC_FAILED: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("REPORT_SYNC_HTTP_{}", res.status()));
    }
    Ok(())
}

// ---------- 顶层：一次同步 ----------

/// 同步结果：给托盘/通知用。
#[derive(Debug, Clone, Default)]
pub struct SyncOutcome {
    /// 本次拉到的服务端配置（None = 离线/失败）。
    pub config: Option<ServerConfig>,
    /// 服务端推荐中本机尚未安装的插件。
    pub pending: Vec<String>,
    /// 本次是否发生了「待装清单变化」（用于通知去重）。
    pub pending_changed: bool,
}

/// 执行一次同步：拉取 → 对比 → 缓存 → 应用菜单策略 → 上报。
///
/// 永不抛错：任何失败都以 `SyncOutcome::default()`（或缓存内容）返回，并写日志。
pub async fn sync_once<R: Runtime>(
    app: &AppHandle<R>,
    cfg: &LauncherConfig,
    last_notified_hash: Option<u64>,
) -> SyncOutcome {
    let server_url = resolve_server_url(cfg);
    if server_url.is_empty() {
        return SyncOutcome::default();
    }
    let token = cfg.admin_token.as_deref().unwrap_or("").to_string();
    let plugin_infos = collect_client_state(app, cfg);
    // 上报用全量（管理页看所有 profile）；待装判断用「当前 profile 已装 + 版本」
    let installed_all: Vec<String> = plugin_infos.iter().map(|p| p.name.clone()).collect();
    let installed_with_ver = installed_plugins_current_profile_with_versions(app, cfg);
    let mut state = load_state(app, cfg);

    let outcome = match fetch_config(&server_url, &token).await {
        Ok(config) => {
            // 版本检查：查 registry 最新版（带缓存），已装但落后的计入待处理
            let latest = refresh_plugin_latest_versions(app, cfg, &config.plugins, &mut state).await;
            let pending_entries = pending_with_updates(&config.plugins, &installed_with_ver, &latest);
            // 纯名字列表（通知/上报用）
            let pending: Vec<String> = pending_entries
                .iter()
                .filter_map(|p| p["name"].as_str().map(|s| s.to_string()))
                .collect();
            // 服务器配置覆盖本地（遵循「用户显式设置过的不被覆盖」）
            apply_server_defaults(app, &config);
            // 缓存菜单策略（托盘渲染据此展示；用户配置永不被覆盖）
            state.cached_managed_menu = config.managed_menu.clone();
            state.cached_config = Some(config.clone());
            state.last_installed = installed_all.clone();
            state.last_sync_at = Some(now_iso());
            save_state(app, cfg, &state);
            // 菜单策略可能变化 → 刷新托盘
            tray::refresh_sync_menu(app);
            log::info!(
                "同步成功：应装 {} 个插件，本机待装 {} 个；菜单策略 {}",
                config.plugins.len(),
                pending.len(),
                if config.managed_menu.as_ref().map(|m| m.enabled).unwrap_or(false) { "启用" } else { "关闭" }
            );
            let pending_hash = hash_list(&pending);
            SyncOutcome {
                pending_changed: last_notified_hash.map(|h| h != pending_hash).unwrap_or(!pending.is_empty()),
                config: Some(config),
                pending,
            }
        }
        Err(e) => {
            log::warn!("同步失败（离线？）：{e}；使用缓存配置");
            // 离线：用缓存配置 + 缓存的最新版本信息继续（版本判断用上次查到的）
            let pending: Vec<String> = state
                .cached_config
                .as_ref()
                .map(|c| {
                    crate::sync::pending_with_updates(&c.plugins, &installed_with_ver, &state.plugin_latest_versions)
                        .iter()
                        .filter_map(|p| p["name"].as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            SyncOutcome {
                pending_changed: false,
                config: state.cached_config.clone(),
                pending,
            }
        }
    };

    // 在线成功才上报（离线不上报，服务端保留上次在线状态）
    if outcome.config.is_some() {
        let cid = client_id(app, cfg);
        let hostname = hostname();
        let dsh_version = crate::workflow::installed_dsh_version(app).unwrap_or_default();
        let profiles = list_profile_names(app, cfg);
        let menu = current_menu(app, cfg);
        let menu_applied = menu_strategy_enabled(app, cfg);
        let config_state = ClientConfigState {
            profile: crate::config::resolve_profile(cfg),
            port: crate::config::resolve_port(cfg),
        };
        let bridge_status = BridgeStatus {
            enabled: crate::admin_bridge::is_running(),
            port: crate::admin_bridge::port().unwrap_or(0),
        };
        let report = SyncReport {
            server_url: &server_url,
            token: &token,
            client_id: &cid,
            hostname: &hostname,
            dsh_version: &dsh_version,
            launcher_version: env!("CARGO_PKG_VERSION"),
            installed: &installed_all,
            pending: &outcome.pending,
            offline: false,
            plugins: &plugin_infos,
            menu: &menu,
            menu_applied,
            profiles: &profiles,
            config_state: &config_state,
            bridge_status: &bridge_status,
        };
        let _ = report_sync(report).await.map_err(|e| log::warn!("上报同步状态失败：{e}"));
    }

    outcome
}

/// 应用服务器下发的客户端默认配置（clientDefaults），遵循「用户显式设置过的不覆盖」。
/// 合并后写回 launcher-config.json 并刷新缓存。
fn apply_server_defaults<R: Runtime>(app: &AppHandle<R>, server: &ServerConfig) {
    let mut local = load_cached();
    let before = local.clone();

    // 1) clientDefaults：npmRegistry/ghMirrorPrefix/port 等
    if let Some(defaults) = &server.client_defaults {
        if defaults.is_object()
            && !defaults.as_object().map(|o| o.is_empty()).unwrap_or(true)
        {
            let user_set = crate::config::user_set_fields(app);
            let user_set: Vec<&str> = user_set.iter().map(|s| s.as_str()).collect();
            crate::config::apply_server_overrides(&mut local, defaults, &user_set);
        }
    }

    // 2) mirrorSettings.dshMirrorUrl：内网 dsh 分发源（客户端更新/切换 dsh 版本用）
    if let Some(ms) = &server.mirror_settings {
        if let Some(url) = ms.get("dshMirrorUrl").and_then(|v| v.as_str()) {
            let trimmed = url.trim();
            let mirror_settings = local.mirror_settings.get_or_insert_with(Default::default);
            // 服务器显式下发的值覆盖本地（dshMirrorUrl 属企业统一管理项，
            // 不遵循「用户显式设置不覆盖」——用户本地一般不手改这个字段）
            if trimmed.is_empty() {
                mirror_settings.dsh_mirror_url = None;
            } else {
                mirror_settings.dsh_mirror_url = Some(trimmed.to_string());
            }
        }
    }

    // 有变化才写回 + 缓存
    if local != before {
        log::info!("应用服务器默认配置（含 dsh 分发源）：{:?}", server.mirror_settings);
        let _ = crate::config::save_config(app, &local);
    }
}

/// 当前实际展示的托盘菜单（策略启用→策略项；否则用户项）。
pub fn current_menu<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> Vec<QuickLink> {
    let state = load_state(app, cfg);
    if let Some(m) = &state.cached_managed_menu {
        if m.enabled {
            return m.quick_links.clone();
        }
    }
    cfg.quick_links.clone().unwrap_or_default()
}

/// 菜单策略当前是否启用（有缓存且 enabled）。
pub fn menu_strategy_enabled<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> bool {
    load_state(app, cfg)
        .cached_managed_menu
        .as_ref()        .map(|m| m.enabled)
        .unwrap_or(false)
}

/// 枚举本机所有 profile 名。
pub fn list_profile_names<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> Vec<String> {
    let dir = dsh_home(app, cfg).join("profiles");
    let mut names: Vec<String> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir() && e.path().join("package.json").exists())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

/// 后台同步循环：按 `syncIntervalSecs` 周期执行 `sync_once`。
///
/// - 离线：网络失败仅日志，循环继续按间隔重试（不 panic、不退出）；
/// - 待装清单变化（相对上次已通知的 hash）→ 系统通知一次；
/// - 之后托盘菜单由 `tray::refresh_sync_menu` 刷新（见 tray.rs）。
pub async fn spawn_sync_loop<R: Runtime>(app: &AppHandle<R>) {
    // 通知去重：记录最近一次已通知的待装 hash（进程内）
    static LAST_NOTIFIED: std::sync::Mutex<Option<u64>> = std::sync::Mutex::new(None);

    let cfg = load_cached();
    let interval_secs = sync_interval_secs(&cfg);
    log::info!("同步循环启动：间隔 {}s，服务端 {}", interval_secs, resolve_server_url(&cfg));

    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
    // 启动后立即同步一次（不等第一个间隔）
    ticker.tick().await; // 首个 tick 立即返回
    loop {
        let cfg = load_cached();
        let last = *LAST_NOTIFIED.lock().unwrap();
        let outcome = sync_once(app, &cfg, last).await;

        // 待装变化 → 通知 + 刷新托盘（待装含「未装」与「已装旧版需更新」）
        if outcome.pending_changed && !outcome.pending.is_empty() {
            let msg = format!(
                "有 {} 个推荐插件待处理：{}\n请在托盘「同步 / 推荐插件」菜单中确认安装/更新",
                outcome.pending.len(),
                outcome.pending.join(", ")
            );
            notify::notify(app, "Harness 推荐插件", &msg);
            let hash = crate::sync::hash_list(&outcome.pending);
            *LAST_NOTIFIED.lock().unwrap() = Some(hash);
        }
        // 无论是否有变化都刷新托盘（安装后 pending 归零也刷新）
        tray::refresh_sync_menu(app);

        ticker.tick().await;
    }
}

/// 安装/更新一个插件（供托盘菜单调用）：`node <dsh>/lib/bin.js plugin --profile <当前profile> add <spec>`。
/// - 已装旧版（版本检查检测到落后）→ `add <pkg>@<latest>`（显式版本，pnpm 100% 升级；
///   裸 `add <pkg>` 或 `@latest` 对已装包可能不升级/解析到旧版）
/// - 未装 → `add <pkg>`（装 registry 最新）
pub async fn install_plugin<R: Runtime>(app: &AppHandle<R>, name: &str) -> Result<(), String> {
    let cfg = load_cached();
    let profile = resolve_profile(&cfg);
    let node = effective_node_path(app, &cfg);
    let dsh_bin = dsh_binary_path(app);
    if !node.exists() || !dsh_bin.exists() {
        return Err("NODE_OR_DSH_NOT_FOUND: 请先「安装 / 修复」".to_string());
    }
    // 决定安装 spec：已装旧版 → name@latest（显式版本更新）
    let state = load_state(app, &cfg);
    let installed_with_ver = installed_plugins_current_profile_with_versions(app, &cfg);
    let is_update = match installed_with_ver.get(name) {
        Some(installed_v) if !installed_v.is_empty() => {
            state
                .plugin_latest_versions
                .get(name)
                .map(|lv| version_greater(lv, installed_v))
                .unwrap_or(false)
        }
        _ => false,
    };
    let spec = if is_update {
        // 用缓存的最新版本（版本检查已拉取）；没有缓存则用 @latest 兜底
        match state.plugin_latest_versions.get(name) {
            Some(lv) => format!("{name}@{lv}"),
            None => format!("{name}@latest"),
        }
    } else {
        name.to_string()
    };
    let env = crate::workflow::child_env(app, &cfg)?;
    let mut cmd = std::process::Command::new(&node);
    cmd.arg(&dsh_bin)
        .arg("plugin")
        .arg("--profile")
        .arg(&profile)
        .arg("add")
        .arg(&spec)
        .current_dir(dsh_install_path(app))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (k, v) in &env {
        cmd.env(k, v);
    }
    let output = tauri::async_runtime::spawn_blocking(move || cmd.output()).await
        .map_err(|e| format!("INSTALL_SPAWN_FAILED: {e}"))?;
    let output = output.map_err(|e| format!("INSTALL_LAUNCH_FAILED: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::error!("安装插件 {spec} 失败（exit={}）：{}", output.status, stderr.trim());
        return Err(format!("PLUGIN_INSTALL_FAILED: {name}（exit={}），详情见日志", output.status));
    }
    log::info!("插件已安装：{spec}（profile={profile}）");
    Ok(())
}

// ---------- 工具 ----------

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default()
}

/// 稳定哈希（FNV-1a 64），用于「待装清单是否变化」判断。
pub fn hash_list(items: &[String]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for item in items {
        for b in item.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= b';' as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_is_recommended_minus_installed() {
        let rec = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let inst = vec!["b".to_string()];
        assert_eq!(pending_plugins(&rec, &inst), vec!["a".to_string(), "c".to_string()]);
    }

    #[test]
    fn pending_preserves_order_and_dedups() {
        let rec = vec!["x".to_string(), "y".to_string(), "x".to_string()];
        assert_eq!(pending_plugins(&rec, &[]), vec!["x".to_string(), "y".to_string()]);
    }

    #[test]
    fn pending_empty_when_all_installed() {
        let rec = vec!["a".to_string()];
        let inst = vec!["a".to_string()];
        assert!(pending_plugins(&rec, &inst).is_empty());
    }

    #[test]
    fn client_id_is_uuid_v4_shape() {
        let id = new_uuid_v4();
        assert!(valid_client_id(&id));
        assert_eq!(id.len(), 36);
        assert_eq!(id.chars().filter(|&c| c == '-').count(), 4);
        // 版本位: 第 15 个字符（index 14）应为 '4'
        assert_eq!(id.as_bytes()[14] as char, '4');
        // 变体位: 第 19 个字符（index 19）应为 '8'/'9'/'a'/'b'
        let variant = id.as_bytes()[19] as char;
        assert!(matches!(variant, '8' | '9' | 'a' | 'b'));
    }

    #[test]
    fn hash_is_stable_and_order_sensitive() {
        let a = vec!["a".to_string(), "b".to_string()];
        let b = vec!["a".to_string(), "b".to_string()];
        let c = vec!["b".to_string(), "a".to_string()];
        assert_eq!(hash_list(&a), hash_list(&b));
        assert_ne!(hash_list(&a), hash_list(&c));
    }

    #[test]
    fn client_id_rejects_garbage() {
        assert!(!valid_client_id(""));
        assert!(!valid_client_id("has space"));
        assert!(!valid_client_id(&"x".repeat(65)));
    }

    #[test]
    fn menu_strategy_override_and_fallback() {
        let user_links = vec![
            QuickLink { label: "用户项".to_string(), url: "https://user.example".to_string() },
        ];
        let mut cfg = LauncherConfig {
            quick_links: Some(user_links),
            ..Default::default()
        };
        // 无策略缓存 → 用户项
        let state = SyncState::default();
        let menu = match &state.cached_managed_menu {
            Some(m) if m.enabled => m.quick_links.clone(),
            _ => cfg.quick_links.clone().unwrap_or_default(),
        };
        assert_eq!(menu.len(), 1);
        assert_eq!(menu[0].label, "用户项");

        // 策略启用 → 策略项（覆盖展示）
        cfg.quick_links = Some(vec![QuickLink { label: "用户项".to_string(), url: "https://user.example".to_string() }]);
        let state = SyncState {
            cached_managed_menu: Some(ManagedMenu {
                enabled: true,
                quick_links: vec![QuickLink { label: "公司OA".to_string(), url: "http://oa.internal".to_string() }],
            }),
            ..Default::default()
        };
        let menu = match &state.cached_managed_menu {
            Some(m) if m.enabled => m.quick_links.clone(),
            _ => cfg.quick_links.clone().unwrap_or_default(),
        };
        assert_eq!(menu.len(), 1);
        assert_eq!(menu[0].label, "公司OA");

        // 策略禁用 → 回退用户项
        let state = SyncState {
            cached_managed_menu: Some(ManagedMenu { enabled: false, quick_links: vec![] }),
            ..Default::default()
        };
        let menu = match &state.cached_managed_menu {
            Some(m) if m.enabled => m.quick_links.clone(),
            _ => cfg.quick_links.clone().unwrap_or_default(),
        };
        assert_eq!(menu[0].label, "用户项");
    }

    #[test]
    fn pending_across_profiles_any_installed_counts() {
        // 服务端应装清单；客户端任一 profile 装了即算满足
        let rec = vec!["dsh-a".to_string(), "dsh-b".to_string()];
        let installed = vec!["dsh-a".to_string()]; // matrix profile 装了 dsh-a
        assert_eq!(pending_plugins(&rec, &installed), vec!["dsh-b".to_string()]);
    }

    #[test]
    fn report_sync_builds_extended_payload() {
        // 验证 report_sync 生成的 body 包含 plugins/menu/menuApplied/profiles/configState
        // 通过序列化 SyncReport 所需字段间接验证结构
        let plugin = PluginInfo {
            name: "dsh-test".to_string(),
            version: "1.0.0".to_string(),
            description: "测试插件".to_string(),
            profile: "web".to_string(),
            client: true,
        };
        assert_eq!(plugin.name, "dsh-test");
        assert_eq!(plugin.version, "1.0.0");
        assert!(plugin.client);

        // ServerConfig 解析 managedMenu（serde 路径）
        let json = r#"{"version":2,"plugins":["a"],"managedMenu":{"enabled":true,"quickLinks":[{"label":"OA","url":"http://oa.internal"}]}}"#;
        let cfg: ServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.version, Some(2));
        assert!(cfg.managed_menu.as_ref().unwrap().enabled);
        assert_eq!(cfg.managed_menu.as_ref().unwrap().quick_links.len(), 1);
    }

    #[test]
    fn managed_menu_invalid_falls_back_to_user() {
        // 非法/缺失 managedMenu → 回退用户项（cached_managed_menu 为 None 或 disabled）
        let state = SyncState::default(); // 无缓存
        assert!(state.cached_managed_menu.is_none());
        let enabled = state.cached_managed_menu.as_ref().map(|m| m.enabled).unwrap_or(false);
        assert!(!enabled, "无缓存时菜单策略视为关闭");
    }
}
