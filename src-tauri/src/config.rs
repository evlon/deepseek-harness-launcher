//! 启动器配置、路径与加速源解析。
//!
//! 配置存于 `<app_data>/launcher-config.json`（用户可编辑，重启生效）。全部字段可选，
//! 缺省按地域自动选择下载/加速源。与现有桌面端隔离：依赖装在 `<app_data>` 内，
//! Harness 用户数据（`$DSH_HOME`）默认 `~/.dsh-launcher`，互不影响。

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, Runtime};

/// 下载地域
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// 中国大陆：走镜像（npmmirror / ghfast.top）
    Domestic,
    /// 其他地区：直连官方源
    Overseas,
}

/// 可配置网址菜单项
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuickLink {
    pub label: String,
    pub url: String,
}

/// 启动器配置（JSON，全字段可选）
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LauncherConfig {
    /// Harness 服务端口（缺省 3180，避开桌面端 3080）
    pub port: Option<u16>,
    /// npm registry 列表（按序尝试；第一个写入 .npmrc/托盘显示）。
    /// 兼容旧格式单个字符串；空 = 按地域自动。
    #[serde(default, deserialize_with = "de_string_or_vec")]
    pub npm_registry: Option<Vec<String>>,
    /// GitHub 中转前缀列表（按序尝试生成镜像 URL）。
    /// 兼容旧格式单个字符串；"none"/空 = 直连。
    #[serde(default, deserialize_with = "de_string_or_vec")]
    pub gh_mirror_prefix: Option<Vec<String>>,
    /// 开机自启动
    pub auto_start: Option<bool>,
    /// 自定义 `$DSH_HOME`（Harness 用户数据目录）；空= `~/.dsh-launcher`
    pub dsh_home: Option<String>,
    /// 托盘「常用网址」菜单项
    pub quick_links: Option<Vec<QuickLink>>,
    /// 企业中心服务端地址（如 `http://10.0.0.5:8080`）；空 = 不启用同步。
    pub server_url: Option<String>,
    /// 同步轮询间隔（秒，缺省 300）。
    pub sync_interval_secs: Option<u64>,
    /// 管理 token（可选，拉取/上报时随请求头携带；服务端未设 token 时忽略）。
    pub admin_token: Option<String>,
    /// 启动的 Harness profile（缺省 web）。
    pub profile: Option<String>,
    /// 管理能力（外网代理网关）：管理员 launcher 在本机 127.0.0.1 起本地 API，
    /// 供服务端管理页中转查包信息（服务端不直接出外网）。
    pub admin_bridge: Option<AdminBridgeConfig>,
    /// IP 地域检测（用 ipinfo.io / ip.sb 等获取设备所在地域，替代 locale/时区判断）
    pub geo_detection: Option<GeoDetectionConfig>,
    /// 是否优先使用系统 node（PATH 上版本匹配则跳过下载自带 node）。
    /// 默认 false = 自包含（launcher 装自己的 node，同事机器一致性）；
    /// 开发者可设 true 省下载/省空间。
    pub use_system_node: Option<bool>,
}

/// 是否启用「使用系统 node」（默认 false = 自包含）。
pub fn use_system_node(cfg: &LauncherConfig) -> bool {
    cfg.use_system_node.unwrap_or(false)
}

/// 反序列化：兼容「单个字符串」与「字符串数组」两种格式。
/// `"https://a/"` → `["https://a/"]`；`["https://a/", "https://b/"]` → 原样。
fn de_string_or_vec<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(vec![trimmed.to_string()]))
            }
        }
        serde_json::Value::Array(arr) => {
            let mut out = Vec::new();
            for v in arr {
                if let serde_json::Value::String(s) = v {
                    let t = s.trim();
                    if !t.is_empty() {
                        out.push(t.to_string());
                    }
                }
            }
            if out.is_empty() {
                Ok(None)
            } else {
                Ok(Some(out))
            }
        }
        serde_json::Value::Null => Ok(None),
        _ => Err(D::Error::custom("expected string or array of strings")),
    }
}

/// IP 地域检测配置。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoDetectionConfig {
    /// 是否启用（默认 true；关闭则用系统 locale/时区判断）
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 检测服务 URL（默认 https://ipinfo.io/json，可换 https://api.ip.sb/geoip 或自定义）
    #[serde(default)]
    pub provider: Option<String>,
}

fn default_true() -> bool {
    true
}

/// IP 地域检测默认服务。
pub const DEFAULT_GEO_PROVIDER: &str = "https://ipinfo.io/json";
/// 备用检测服务（默认 provider 不可用时尝试）。
pub const FALLBACK_GEO_PROVIDER: &str = "https://api.ip.sb/geoip";

/// 解析 IP 检测服务 URL（配置优先，缺省 ipinfo.io）。
pub fn geo_provider(cfg: &LauncherConfig) -> String {
    cfg.geo_detection
        .as_ref()
        .and_then(|g| g.provider.clone())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| DEFAULT_GEO_PROVIDER.to_string())
}

/// IP 地域检测是否启用。
pub fn geo_detection_enabled(cfg: &LauncherConfig) -> bool {
    cfg.geo_detection
        .as_ref()
        .map(|g| g.enabled)
        .unwrap_or(true)
}

/// 管理能力（外网代理网关）配置。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminBridgeConfig {
    /// 是否开启（托盘「管理能力」可切换）
    #[serde(default)]
    pub enabled: bool,
    /// 监听端口（缺省 3410；占用自动顺延）
    #[serde(default)]
    pub port: Option<u16>,
    /// 本地 API token（管理页 fetch 时带 X-Bridge-Token；空 = 不校验）
    #[serde(default)]
    pub token: Option<String>,
    /// 是否允许执行服务端下发的脚本（默认关闭，安全）
    #[serde(default)]
    pub allow_scripts: bool,
}

/// 管理能力默认端口（避开 3080/3180 等）。
pub const DEFAULT_BRIDGE_PORT: u16 = 3410;

// ---------- 常量（参照 deepseek-harness-desktop 的 constants.rs） ----------

pub const NODE_VERSION: &str = "v22.22.0";
pub const NODE_BASE_URL: &str = "https://nodejs.org/dist/";
pub const NODE_MIRROR_BASE_URL: &str = "https://npmmirror.com/mirrors/node/";

pub const DSH_CORE_URL: &str =
    "https://github.com/dsh-tauri-desk/deepseek-harness-pkg/releases/latest/download/";
pub const DSH_MIRROR_PREFIX: &str = "https://ghfast.top/";

pub const PNPM_VERSION: &str = "11.7.0";
pub const PNPM_SHA256: &str = "deafa7ec98a1218b6a047289b92fbe2395c1e22d3495bb711653013218ee15ee";
pub const PNPM_BASE_URL: &str = "https://registry.npmjs.org/pnpm/-/";
pub const PNPM_MIRROR_BASE_URL: &str = "https://registry.npmmirror.com/pnpm/-/";

pub const MINGIT_VERSION: &str = "2.53.0.2";
pub const MINGIT_X64_SHA256: &str =
    "d4bf83d6a860ccae9af44e508e1e00a39f09db6fa78a9ba5543b94d87ca22a29";
pub const MINGIT_BASE_URL: &str =
    "https://github.com/git-for-windows/git/releases/download/v2.53.0.windows.2/";

pub const OFFICIAL_NPM_REGISTRY: &str = "https://registry.npmjs.org/";
pub const NPM_MIRROR_REGISTRY: &str = "https://registry.npmmirror.com/";

/// Launcher 启动的 Harness 服务默认端口。
///
/// ⚠️ 刻意避开 3080（那是桌面端/日常工作的 dsh web 常用端口）：
/// launcher 的 `$DSH_HOME`（~/.dsh-launcher）与桌面端（~/.dsh）隔离，
/// 端口也必须独立，避免与同事/自己的工作实例冲突。
pub const DEFAULT_PORT: u16 = 3180;

// ---------- 路径 ----------

/// 启动器专属 App Data 目录（Tauri 按 identifier 隔离，天然与现有桌面端不同）。
pub fn base_dir<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    app.path()
        .app_data_dir()
        .expect("Failed to resolve app data directory")
}

pub fn config_path<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    base_dir(app).join("launcher-config.json")
}

pub fn logs_dir<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    base_dir(app).join("logs")
}

pub fn log_file<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    logs_dir(app).join("launcher.log")
}

pub fn runtime_path<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    base_dir(app).join("runtime")
}

pub fn dsh_install_path<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    base_dir(app).join("dependencies").join("dsh")
}

pub fn pnpm_install_path<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    base_dir(app).join("dependencies").join("pnpm")
}

#[cfg(windows)]
pub fn git_install_path<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    base_dir(app).join("dependencies").join("git")
}

pub fn node_binary_path<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    let runtime = runtime_path(app);
    if cfg!(windows) {
        runtime.join("node.exe")
    } else {
        runtime.join("bin").join("node")
    }
}

/// 实际生效的 node 路径：
/// - `useSystemNode=true` 且 PATH 上系统 node 版本匹配 → 系统 node
/// - 否则 → launcher 自带的 node
pub fn effective_node_path<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> PathBuf {
    if use_system_node(cfg) {
        if let Some(system) = system_node_on_path() {
            if node_version_matches(&system) {
                log::info!("启动使用系统 node：{}", system.display());
                return system;
            }
        }
    }
    node_binary_path(app)
}

/// PATH 上的系统 node 路径（Windows: node.exe；Unix: node）。
fn system_node_on_path() -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| {
            if cfg!(windows) {
                dir.join("node.exe")
            } else {
                dir.join("node")
            }
        })
        .find(|p| p.is_file())
}

/// 执行 node --version 并比对版本：主版本 >= NODE_MIN_MAJOR 即视为可用。
///
/// 版本匹配放宽为「主版本 >= 期望」，而非精确相等——node 22+ 向后兼容，
/// 系统装了更新的 node（如 v24）应直接复用，避免无谓下载。
fn node_version_matches(bin: &Path) -> bool {
    if !bin.exists() {
        return false;
    }
    let Ok(output) = std::process::Command::new(bin)
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let version = String::from_utf8_lossy(&output.stdout);
    node_version_compatible(&version)
}

/// 版本兼容性判断：`v22.22.0` → 主版本 22 >= 期望主版本（22）→ true。
/// 期望版本取 `NODE_VERSION` 常量（形如 "v22.22.0"）。
fn node_version_compatible(version: &str) -> bool {
    let min_major = NODE_VERSION
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(22);
    let ver_major = version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    ver_major >= min_major
}

pub fn dsh_binary_path<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    dsh_install_path(app).join("node_modules/@deepseek-ai/dsh/lib/bin.js")
}

pub fn pnpm_binary_path<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    pnpm_install_path(app).join("bin/pnpm.cjs")
}

#[cfg(windows)]
pub fn git_binary_path<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    git_install_path(app).join("cmd/git.exe")
}

/// Harness 用户数据目录（`$DSH_HOME`）。缺省 `~/.dsh-launcher`，可被配置覆盖。
pub fn dsh_home<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> PathBuf {
    if let Some(home) = &cfg.dsh_home {
        if !home.is_empty() {
            return PathBuf::from(home);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| base_dir(app))
        .join(".dsh-launcher")
}

pub fn resolve_port(cfg: &LauncherConfig) -> u16 {
    cfg.port.unwrap_or(DEFAULT_PORT)
}

/// 中心服务端地址（去空格；空 = 未配置同步）。
pub fn resolve_server_url(cfg: &LauncherConfig) -> String {
    cfg.server_url.as_deref().unwrap_or("").trim().to_string()
}

/// 同步轮询间隔（秒），缺省 300。
pub fn sync_interval_secs(cfg: &LauncherConfig) -> u64 {
    cfg.sync_interval_secs.unwrap_or(300).max(30)
}

/// 启动的 Harness profile（缺省 web）。过滤掉非法名字（路径穿越等）。
pub fn resolve_profile(cfg: &LauncherConfig) -> String {
    let p = cfg.profile.as_deref().unwrap_or("web").trim();
    if p.is_empty() || p.contains('/') || p.contains('\\') || p == "." || p == ".." {
        "web".to_string()
    } else {
        p.to_string()
    }
}

/// 枚举 `$DSH_HOME/profiles/` 下已存在的 profile 名（排序）。
pub fn list_profiles<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> Vec<String> {
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

/// 写入配置中的 profile 字段。
pub fn set_profile<R: Runtime>(app: &AppHandle<R>, profile: &str) -> Result<(), String> {
    let mut cfg = load_cached();
    cfg.profile = Some(profile.to_string());
    save_config(app, &cfg)
}

/// 管理能力是否开启（配置里 enabled）。
pub fn bridge_enabled(cfg: &LauncherConfig) -> bool {
    cfg.admin_bridge.as_ref().map(|b| b.enabled).unwrap_or(false)
}

/// 管理能力端口（缺省 DEFAULT_BRIDGE_PORT）。
pub fn bridge_port(cfg: &LauncherConfig) -> u16 {
    cfg.admin_bridge
        .as_ref()
        .and_then(|b| b.port)
        .unwrap_or(DEFAULT_BRIDGE_PORT)
}

/// 管理能力 token（空 = 不校验）。
pub fn bridge_token(cfg: &LauncherConfig) -> String {
    cfg.admin_bridge
        .as_ref()
        .and_then(|b| b.token.clone())
        .unwrap_or_default()
}

/// 设置管理能力开关（托盘「开启/关闭管理能力」）。
pub fn set_bridge_enabled<R: Runtime>(app: &AppHandle<R>, enabled: bool) -> Result<(), String> {
    let mut cfg = load_cached();
    let mut bridge = cfg.admin_bridge.clone().unwrap_or_default();
    bridge.enabled = enabled;
    cfg.admin_bridge = Some(bridge);
    save_config(app, &cfg)
}

// ---------- 地域检测（移植自 deepseek-harness-desktop 的 config/region.rs） ----------

/// 判定当前下载地域（带缓存，进程生命周期内恒定）。
///
/// Windows 读取系统默认区域（`GetUserDefaultLocaleName`）与动态时区
/// （`GetDynamicTimeZoneInformation`，返回 `China Standard Time`），
/// 任一击中大陆即走镜像；Unix 读 `LC_ALL/LC_MESSAGES/LANG` 与 `/etc/localtime`。
///
/// 启动时 `detect_region_async` 会用 IP 地理定位（ipinfo.io/ip.sb）覆盖该缓存；
/// 此处是同步 fallback（locale/时区），供 IP 检测未完成或失败时使用。
pub fn detect_region() -> Region {
    *region_cache().lock().unwrap()
}

/// 地域缓存（初始 = locale/时区判断；IP 检测成功后覆盖）。
fn region_cache() -> &'static std::sync::Mutex<Region> {
    static REGION: once_cell::sync::OnceCell<std::sync::Mutex<Region>> = once_cell::sync::OnceCell::new();
    REGION.get_or_init(|| {
        let locale = current_locale();
        let locale_zh = locale_is_china(&locale);
        let tz_china = is_china_timezone();
        let region = region_for(locale_zh, tz_china);
        log::info!(
            "Download region detected: {:?} (locale={locale:?}, china_timezone={tz_china})",
            region
        );
        std::sync::Mutex::new(region)
    })
}

/// 用 IP 地理定位结果覆盖地域缓存（async 检测成功后调用）。
pub fn set_detected_region(region: Region, provider: &str, country: &str) {
    *region_cache().lock().unwrap() = region;
    log::info!(
        "Download region updated by IP geo: {:?} (country={country:?}, provider={provider:?})",
        region
    );
}

/// 异步 IP 地域检测：查配置的 provider（缺省 ipinfo.io），失败尝试备用 ip.sb，
/// 再失败保留 locale fallback。返回最终 Region（可能未变化）。
///
/// 绝不 panic：任何网络错误都回退 locale 判断。
pub async fn detect_region_async(cfg: &LauncherConfig) -> Region {
    if !geo_detection_enabled(cfg) {
        log::info!("IP 地域检测已关闭，使用 locale/时区判断");
        return detect_region();
    }
    let primary = geo_provider(cfg);
    let providers: Vec<&str> = if primary == DEFAULT_GEO_PROVIDER {
        vec![DEFAULT_GEO_PROVIDER, FALLBACK_GEO_PROVIDER]
    } else {
        vec![primary.as_str()]
    };

    let client = reqwest::Client::builder()
        .user_agent("deepseek-harness-launcher-geo")
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(6))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    for provider in providers {
        match query_country(&client, provider).await {
            Ok(Some(country)) => {
                let region = region_from_country(&country);
                set_detected_region(region, provider, &country);
                return region;
            }
            Ok(None) => log::warn!("IP 地域检测 {provider} 未返回有效 country，尝试下一个"),
            Err(e) => log::warn!("IP 地域检测 {provider} 失败：{e}，尝试下一个"),
        }
    }
    log::warn!("IP 地域检测全部失败，回退 locale/时区判断");
    detect_region()
}

/// 查询单个 IP 地理定位服务，返回 country 字符串。
async fn query_country(client: &reqwest::Client, provider: &str) -> Result<Option<String>, String> {
    let res = client
        .get(provider)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("HTTP {}", res.status()));
    }
    let json: serde_json::Value = res.json().await.map_err(|e| format!("parse failed: {e}"))?;
    // ipinfo.io: { "country": "CN" }；ip.sb: { "country_code": "CN", "country": "China" }
    let country = json
        .get("country")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| json.get("country_code").and_then(|v| v.as_str()).map(|s| s.to_string()));
    Ok(country)
}

/// 由 country 判定地域：CN / China / 中国 = Domestic，其他 = Overseas。
fn region_from_country(country: &str) -> Region {
    let c = country.trim().to_ascii_lowercase();
    if c == "cn" || c == "china" || c == "中国" || c.contains("zh-cn") {
        Region::Domestic
    } else {
        Region::Overseas
    }
}

/// 组合判定：简体中文界面语言或中国时区任一命中，即视为国内用户。
fn region_for(locale_zh: bool, tz_china: bool) -> Region {
    if locale_zh || tz_china {
        Region::Domestic
    } else {
        Region::Overseas
    }
}

/// 界面语言是否为大陆简体中文（zh-CN / zh_CN / zh-Hans-CN / zh-Hans）。
///
/// zh-TW / zh-HK / zh-SG 不命中：这些地区的 GitHub 直连通常可用，
/// 保守起见只对大陆用户启用镜像。
fn locale_is_china(locale: &str) -> bool {
    let normalized = locale.to_ascii_lowercase().replace('_', "-");
    normalized.starts_with("zh-cn") || normalized.starts_with("zh-hans")
}

/// 时区名是否指向中国大陆（Asia/Shanghai 及别名 PRC / China/* / China Standard Time）。
fn tz_name_is_china(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase();
    normalized.contains("asia/shanghai")
        || normalized == "prc"
        || normalized.starts_with("prc/")
        || normalized.starts_with("china/")
        || normalized.contains("china standard time")
}

/// 系统界面语言（RFC 语言标签，如 `zh-CN` / `zh_CN`；取不到时返回空串）。
fn current_locale() -> String {
    #[cfg(windows)]
    {
        // Windows 的权威来源是用户默认区域设置（zh-CN / zh-Hans-CN）
        use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
        let mut buf = [0u16; 85];
        // 返回值含结尾 NUL 的字符数；失败返回 0
        let len = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
        if len <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..len as usize - 1])
    }
    #[cfg(not(windows))]
    {
        // LC_ALL > LC_MESSAGES > LANG，去掉编码与修饰符（zh_CN.UTF-8 → zh_CN）
        for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(value) = std::env::var(var) {
                let base = value
                    .split('.')
                    .next()
                    .unwrap_or(&value)
                    .split('@')
                    .next()
                    .unwrap_or(&value);
                if !base.is_empty() && base != "C" && base != "POSIX" {
                    return base.to_string();
                }
            }
        }
        String::new()
    }
}

/// 系统时区是否为中国大陆时区（Asia/Shanghai / China Standard Time）。
fn is_china_timezone() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Time::{
            GetDynamicTimeZoneInformation, DYNAMIC_TIME_ZONE_INFORMATION, TIME_ZONE_ID_INVALID,
        };
        let mut info: DYNAMIC_TIME_ZONE_INFORMATION = unsafe { std::mem::zeroed() };
        let id = unsafe { GetDynamicTimeZoneInformation(&mut info) };
        if id == TIME_ZONE_ID_INVALID {
            return false;
        }
        // TimeZoneKeyName 是定长 UTF-16 数组，取到首个 NUL 为止
        let len = info
            .TimeZoneKeyName
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(info.TimeZoneKeyName.len());
        tz_name_is_china(&String::from_utf16_lossy(&info.TimeZoneKeyName[..len]))
    }
    #[cfg(not(windows))]
    {
        // 1) TZ 环境变量（少数系统显式设置，如 Asia/Shanghai）
        if let Ok(tz) = std::env::var("TZ") {
            if tz_name_is_china(&tz) {
                return true;
            }
        }
        // 2) /etc/localtime 软链：主流发行版指向 /usr/share/zoneinfo/<Area>/<Zone>
        if let Ok(target) = std::fs::read_link("/etc/localtime") {
            if tz_name_is_china(&target.to_string_lossy()) {
                return true;
            }
        }
        // 3) /etc/timezone（Debian/Ubuntu 的纯文本时区名，如 Asia/Shanghai）
        if let Ok(content) = std::fs::read_to_string("/etc/timezone") {
            if tz_name_is_china(content.trim()) {
                return true;
            }
        }
        false
    }
}

// ---------- 加速源解析 ----------

fn ensure_trailing_slash(s: &str) -> String {
    if s.ends_with('/') {
        s.to_string()
    } else {
        format!("{s}/")
    }
}

/// 解析实际生效的 npm registry 列表（按序尝试；空 = 按地域自动）。
pub fn resolve_npm_registries(cfg: &LauncherConfig) -> Vec<String> {
    if let Some(list) = &cfg.npm_registry {
        let cleaned: Vec<String> = list.iter().map(|r| ensure_trailing_slash(r.trim())).collect();
        if !cleaned.is_empty() {
            return cleaned;
        }
    }
    match detect_region() {
        Region::Domestic => vec![NPM_MIRROR_REGISTRY.to_string()],
        Region::Overseas => vec![OFFICIAL_NPM_REGISTRY.to_string()],
    }
}

/// 解析实际生效的 npm registry（第一个，末尾带 `/`；供 .npmrc/托盘显示）。
pub fn resolve_npm_registry(cfg: &LauncherConfig) -> String {
    resolve_npm_registries(cfg)
        .into_iter()
        .next()
        .unwrap_or_else(|| OFFICIAL_NPM_REGISTRY.to_string())
}

/// 解析实际生效的 GitHub 中转前缀列表（按序尝试）；直连返回空 Vec。
///
/// 每项 `"none"`（不区分大小写）按配置文档语义视为「直连」。
pub fn resolve_gh_prefixes(cfg: &LauncherConfig) -> Vec<String> {
    if let Some(list) = &cfg.gh_mirror_prefix {
        let cleaned: Vec<String> = list
            .iter()
            .map(|p| p.trim())
            .filter(|p| !p.is_empty() && !p.eq_ignore_ascii_case("none"))
            .map(ensure_trailing_slash)
            .collect();
        if !cleaned.is_empty() {
            return cleaned;
        }
        return Vec::new(); // 显式直连（none/空）
    }
    match detect_region() {
        Region::Domestic => vec![DSH_MIRROR_PREFIX.to_string()],
        Region::Overseas => Vec::new(),
    }
}

/// 解析实际生效的 GitHub 中转前缀（第一个）；直连返回 `None`。
///
/// `"none"`（不区分大小写）按配置文档语义视为「直连」，
/// 防止手编配置文件写入 `"ghMirrorPrefix": "none"` 时被当成字面前缀。
pub fn resolve_gh_prefix(cfg: &LauncherConfig) -> Option<String> {
    resolve_gh_prefixes(cfg).into_iter().next()
}

/// 为任意 GitHub 资产 URL 生成中转兜底地址列表（每个镜像前缀一个）。
/// 顺序 = 配置顺序；直连时返回空（调用方应只用官方 URL）。
pub fn mirror_urls(asset_url: &str, cfg: &LauncherConfig) -> Vec<String> {
    let prefixes = resolve_gh_prefixes(cfg);
    if prefixes.is_empty() {
        return Vec::new();
    }
    prefixes.iter().map(|p| format!("{p}{asset_url}")).collect()
}

/// 为任意 GitHub 资产 URL 生成中转兜底地址（第一个镜像前缀；直连时返回官方原样）。
pub fn mirror_url(asset_url: &str, cfg: &LauncherConfig) -> String {
    resolve_gh_prefix(cfg)
        .map(|p| format!("{p}{asset_url}"))
        .unwrap_or_else(|| asset_url.to_string())
}

// ---------- 配置读写 ----------

static CACHED: once_cell::sync::OnceCell<std::sync::Mutex<LauncherConfig>> = once_cell::sync::OnceCell::new();

/// 进程内缓存：启动后加载一次，托盘预设写入后同步更新。
pub fn load_cached() -> LauncherConfig {
    CACHED
        .get()
        .map(|m| m.lock().unwrap().clone())
        .unwrap_or_default()
}

fn cache(cfg: LauncherConfig) {
    let cell = CACHED.get_or_init(|| std::sync::Mutex::new(cfg.clone()));
    *cell.lock().unwrap() = cfg;
}

/// 编译内置的默认配置（随二进制分发，缺省值兜底）。
fn builtin_default_config() -> LauncherConfig {
    // include_str! 在编译时嵌入 default-config.json
    serde_json::from_str(include_str!("../default-config.json"))
        .unwrap_or_else(|e| {
            log::warn!("内置默认配置解析失败（不应发生）：{e}");
            LauncherConfig::default()
        })
}

/// 启动时加载配置：内置默认 ⊕ 用户文件（内置兜底，用户覆盖）。
pub fn load_config<R: Runtime>(app: &AppHandle<R>) -> LauncherConfig {
    let path = config_path(app);
    let mut cfg = builtin_default_config();
    match fs::read_to_string(&path) {
        Ok(s) => {
            match serde_json::from_str::<LauncherConfig>(&s) {
                Ok(user_cfg) => merge_user_into_builtin(&mut cfg, user_cfg),
                Err(e) => log::warn!("launcher-config.json 解析失败，使用内置默认：{e}"),
            }
        }
        Err(_) => log::info!("launcher-config.json 不存在，使用内置默认配置"),
    }
    log::info!(
        "Launcher config loaded: port={:?}, npm_registry={:?}, gh_prefix={:?}, dsh_home={:?}, profile={:?}",
        cfg.port, cfg.npm_registry, cfg.gh_mirror_prefix, cfg.dsh_home, cfg.profile
    );
    cache(cfg.clone());
    cfg
}

/// 用户配置覆盖内置默认：用户显式设置的字段（Option=Some）覆盖内置值。
fn merge_user_into_builtin(builtin: &mut LauncherConfig, user: LauncherConfig) {
    if user.port.is_some() { builtin.port = user.port; }
    if user.npm_registry.is_some() { builtin.npm_registry = user.npm_registry; }
    if user.gh_mirror_prefix.is_some() { builtin.gh_mirror_prefix = user.gh_mirror_prefix; }
    if user.auto_start.is_some() { builtin.auto_start = user.auto_start; }
    if user.dsh_home.is_some() { builtin.dsh_home = user.dsh_home; }
    if user.quick_links.is_some() { builtin.quick_links = user.quick_links; }
    if user.server_url.is_some() { builtin.server_url = user.server_url; }
    if user.sync_interval_secs.is_some() { builtin.sync_interval_secs = user.sync_interval_secs; }
    if user.admin_token.is_some() { builtin.admin_token = user.admin_token; }
    if user.profile.is_some() { builtin.profile = user.profile; }
    if user.admin_bridge.is_some() { builtin.admin_bridge = user.admin_bridge; }
    if user.geo_detection.is_some() { builtin.geo_detection = user.geo_detection; }
    if user.use_system_node.is_some() { builtin.use_system_node = user.use_system_node; }
}

/// 服务器配置覆盖本地（遵循「用户显式设置过的不被覆盖」）：
/// `user_set` 记录用户 launcher-config.json 里显式写过的字段名。
/// 返回合并后的配置。
pub fn apply_server_overrides(
    local: &mut LauncherConfig,
    server: &serde_json::Value,
    user_set: &[&str],
) {
    let get = |k: &str| server.get(k).and_then(|v| v.as_str()).map(|s| s.to_string());
    let get_list = |k: &str| -> Option<Vec<String>> {
        let v = server.get(k)?;
        match v {
            serde_json::Value::String(s) => {
                let t = s.trim();
                if t.is_empty() { None } else { Some(vec![t.to_string()]) }
            }
            serde_json::Value::Array(arr) => {
                let out: Vec<String> = arr
                    .iter()
                    .filter_map(|x| x.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if out.is_empty() { None } else { Some(out) }
            }
            _ => None,
        }
    };
    // 仅当用户未显式设置该字段时，才用服务器值覆盖
    if !user_set.contains(&"npmRegistry") {
        if let Some(v) = get_list("npmRegistry") { local.npm_registry = Some(v); }
    }
    if !user_set.contains(&"ghMirrorPrefix") {
        if let Some(v) = get_list("ghMirrorPrefix") { local.gh_mirror_prefix = Some(v); }
    }
    if !user_set.contains(&"port") {
        if let Some(v) = server.get("port").and_then(|v| v.as_u64()) {
            if v >= 1 && v <= 65535 { local.port = Some(v as u16); }
        }
    }
    if !user_set.contains(&"syncIntervalSecs") {
        if let Some(v) = server.get("syncIntervalSecs").and_then(|v| v.as_u64()) {
            if v >= 30 { local.sync_interval_secs = Some(v); }
        }
    }
    if !user_set.contains(&"profile") {
        if let Some(v) = get("profile") {
            if !v.is_empty() && !v.contains('/') && !v.contains('\\') {
                local.profile = Some(v);
            }
        }
    }
    if !user_set.contains(&"useSystemNode") {
        if let Some(v) = server.get("useSystemNode").and_then(|v| v.as_bool()) {
            local.use_system_node = Some(v);
        }
    }
}

/// 记录用户 launcher-config.json 里显式设置的字段名（用于服务器合并时跳过）。
pub fn user_set_fields<R: Runtime>(app: &AppHandle<R>) -> Vec<String> {
    let path = config_path(app);
    let Ok(s) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&s) else {
        return Vec::new();
    };
    json.as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// 写回配置并刷新缓存。
pub fn save_config<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> Result<(), String> {
    let path = config_path(app);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("CONFIG_DIR_CREATE_FAILED: {e}"))?;
    }
    let json = serde_json::to_string_pretty(cfg).map_err(|e| format!("CONFIG_SERIALIZE_FAILED: {e}"))?;
    fs::write(&path, json).map_err(|e| format!("CONFIG_WRITE_FAILED: {e}"))?;
    cache(cfg.clone());
    log::info!("Launcher config saved: {}", path.display());
    Ok(())
}

/// 设置 npm 源预设（空串=auto），写回配置。支持逗号分隔多源。
pub fn set_npm_registry<R: Runtime>(app: &AppHandle<R>, registry: &str) -> Result<(), String> {
    let mut cfg = load_cached();
    cfg.npm_registry = if registry.trim().is_empty() {
        None
    } else {
        Some(
            registry
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        )
    };
    save_config(app, &cfg)
}

/// 设置 GitHub 中转预设（None=auto/直连），写回配置。支持逗号分隔多源。
pub fn set_gh_prefix<R: Runtime>(app: &AppHandle<R>, prefix: Option<&str>) -> Result<(), String> {
    let mut cfg = load_cached();
    cfg.gh_mirror_prefix = match prefix {
        Some(p) if !p.trim().is_empty() => Some(
            p.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        ),
        _ => None,
    };
    save_config(app, &cfg)
}

/// 确保 App Data 基础目录存在（配置/日志/依赖都挂在它下面）。
pub fn ensure_base_dir<R: Runtime>(app: &AppHandle<R>) {
    let _ = fs::create_dir_all(base_dir(app));
    let _ = fs::create_dir_all(logs_dir(app));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_matching_only_matches_mainland_chinese() {
        assert!(locale_is_china("zh-CN"));
        assert!(locale_is_china("zh_CN.UTF-8"));
        assert!(locale_is_china("zh-Hans-CN"));
        assert!(locale_is_china("ZH_cn"));
        // 台湾/香港/新加坡中文不视为大陆
        assert!(!locale_is_china("zh-TW"));
        assert!(!locale_is_china("zh-HK"));
        assert!(!locale_is_china("zh-SG"));
        assert!(!locale_is_china("en-US"));
        assert!(!locale_is_china("ja-JP"));
        assert!(!locale_is_china(""));
    }

    #[test]
    fn timezone_matching_only_matches_china_tz() {
        assert!(tz_name_is_china("Asia/Shanghai"));
        assert!(tz_name_is_china("/usr/share/zoneinfo/Asia/Shanghai"));
        assert!(tz_name_is_china("China Standard Time"));
        assert!(tz_name_is_china("PRC"));
        assert!(!tz_name_is_china("Asia/Singapore"));
        assert!(!tz_name_is_china("Asia/Taipei"));
        assert!(!tz_name_is_china("Asia/Hong_Kong"));
        assert!(!tz_name_is_china("America/New_York"));
        assert!(!tz_name_is_china(""));
    }

    #[test]
    fn region_combination_is_or() {
        assert_eq!(region_for(true, true), Region::Domestic);
        assert_eq!(region_for(true, false), Region::Domestic);
        assert_eq!(region_for(false, true), Region::Domestic);
        assert_eq!(region_for(false, false), Region::Overseas);
    }

    #[test]
    fn npm_registry_auto_follows_region() {
        let auto = LauncherConfig::default();
        let resolved = resolve_npm_registry(&auto);
        match detect_region() {
            Region::Domestic => assert_eq!(resolved, NPM_MIRROR_REGISTRY),
            Region::Overseas => assert_eq!(resolved, OFFICIAL_NPM_REGISTRY),
        }
    }

    #[test]
    fn npm_registry_explicit_overrides_region() {
        let cfg = LauncherConfig {
            npm_registry: Some(vec!["https://registry.example.com".to_string()]),
            ..Default::default()
        };
        assert_eq!(
            resolve_npm_registry(&cfg),
            "https://registry.example.com/"
        );
        // 空列表视为 auto
        let auto = LauncherConfig {
            npm_registry: Some(Vec::new()),
            ..Default::default()
        };
        let _ = resolve_npm_registry(&auto); // 不 panic 即可
    }

    #[test]
    fn gh_prefix_none_means_direct() {
        // 手编配置写 "none"（任意大小写）= 直连
        let cfg = LauncherConfig {
            gh_mirror_prefix: Some(vec!["none".to_string()]),
            ..Default::default()
        };
        assert_eq!(resolve_gh_prefix(&cfg), None);
        let cfg = LauncherConfig {
            gh_mirror_prefix: Some(vec!["NONE".to_string()]),
            ..Default::default()
        };
        assert_eq!(resolve_gh_prefix(&cfg), None);
        // 空列表 = 直连（不启用中转）
        let cfg2 = LauncherConfig {
            gh_mirror_prefix: Some(Vec::new()),
            ..Default::default()
        };
        assert_eq!(resolve_gh_prefix(&cfg2), None);
        // 真实前缀照常生效并补尾斜杠
        let cfg3 = LauncherConfig {
            gh_mirror_prefix: Some(vec!["https://ghfast.top".to_string()]),
            ..Default::default()
        };
        assert_eq!(
            resolve_gh_prefix(&cfg3),
            Some("https://ghfast.top/".to_string())
        );
    }

    #[test]
    fn mirror_urls_prepends_prefixes() {
        let cfg = LauncherConfig {
            gh_mirror_prefix: Some(vec![
                "https://ghfast.top/".to_string(),
                "https://ghproxy.net/".to_string(),
            ]),
            ..Default::default()
        };
        let urls = mirror_urls("https://github.com/a/b.zip", &cfg);
        assert_eq!(
            urls,
            vec![
                "https://ghfast.top/https://github.com/a/b.zip".to_string(),
                "https://ghproxy.net/https://github.com/a/b.zip".to_string(),
            ]
        );
        // 无配置时按地域（本机 Domestic → ghfast.top 兜底）
        let auto = LauncherConfig::default();
        let urls2 = mirror_urls("https://github.com/a/b.zip", &auto);
        if urls2.is_empty() {
            assert!(resolve_gh_prefix(&auto).is_none(), "Overseas 直连无镜像");
        } else {
            assert!(urls2[0].starts_with("https://ghfast.top/"), "got {:?}", urls2[0]);
        }
    }

    #[test]
    fn string_or_vec_deserializes_both() {
        // 旧格式：单个字符串
        let cfg: LauncherConfig = serde_json::from_str(r#"{"npmRegistry":"https://registry.npmmirror.com/"}"#).unwrap();
        assert_eq!(
            cfg.npm_registry,
            Some(vec!["https://registry.npmmirror.com/".to_string()])
        );
        // 新格式：数组
        let cfg2: LauncherConfig = serde_json::from_str(
            r#"{"ghMirrorPrefix":["https://ghfast.top/","https://ghproxy.net/"]}"#,
        )
        .unwrap();
        assert_eq!(cfg2.gh_mirror_prefix, Some(vec![
            "https://ghfast.top/".to_string(),
            "https://ghproxy.net/".to_string(),
        ]));
        // 空字符串 → None
        let cfg3: LauncherConfig = serde_json::from_str(r#"{"npmRegistry":""}"#).unwrap();
        assert_eq!(cfg3.npm_registry, None);
    }

    #[test]
    fn ensure_trailing_slash_normalizes() {
        assert_eq!(ensure_trailing_slash("https://x.com"), "https://x.com/");
        assert_eq!(ensure_trailing_slash("https://x.com/"), "https://x.com/");
    }

    #[test]
    fn country_parsing_detects_region() {
        assert_eq!(region_from_country("CN"), Region::Domestic);
        assert_eq!(region_from_country("cn"), Region::Domestic);
        assert_eq!(region_from_country("China"), Region::Domestic);
        assert_eq!(region_from_country("中国"), Region::Domestic);
        assert_eq!(region_from_country("US"), Region::Overseas);
        assert_eq!(region_from_country("SG"), Region::Overseas);
        assert_eq!(region_from_country(""), Region::Overseas);
    }

    #[test]
    fn builtin_default_config_has_sane_values() {
        let cfg = builtin_default_config();
        assert_eq!(cfg.port, Some(3180), "默认端口应避开 3080");
        assert_eq!(cfg.profile.as_deref(), Some("web"));
        assert!(cfg.geo_detection.as_ref().map(|g| g.enabled).unwrap_or(false), "IP 检测默认开启");
        assert_eq!(geo_provider(&cfg), DEFAULT_GEO_PROVIDER);
    }

    #[test]
    fn user_overrides_builtin() {
        let mut builtin = builtin_default_config();
        let user = LauncherConfig {
            port: Some(4080),
            server_url: Some("http://server.internal".to_string()),
            ..Default::default()
        };
        merge_user_into_builtin(&mut builtin, user);
        assert_eq!(builtin.port, Some(4080));
        assert_eq!(builtin.server_url.as_deref(), Some("http://server.internal"));
        // 未显式设置的字段保留内置默认
        assert_eq!(builtin.profile.as_deref(), Some("web"));
        assert_eq!(builtin.sync_interval_secs, Some(300));
    }

    #[test]
    fn server_overrides_respect_user_explicit_fields() {
        let mut local = builtin_default_config();
        let server = serde_json::json!({
            "npmRegistry": "https://registry.npmmirror.com/",
            "ghMirrorPrefix": "https://ghfast.top/",
            "port": 5000,
            "profile": "matrix",
        });
        // 用户显式设置了 port → 服务器不覆盖 port；其他字段可覆盖
        let user_set = ["port"];
        apply_server_overrides(&mut local, &server, &user_set);
        assert_eq!(local.port, Some(3180), "用户显式设置的 port 不被服务器覆盖");
        assert_eq!(
            local.npm_registry,
            Some(vec!["https://registry.npmmirror.com/".to_string()])
        );
        assert_eq!(
            local.gh_mirror_prefix,
            Some(vec!["https://ghfast.top/".to_string()])
        );
        assert_eq!(local.profile.as_deref(), Some("matrix"));
    }

    #[test]
    fn server_overrides_invalid_values_ignored() {
        let mut local = builtin_default_config();
        let server = serde_json::json!({
            "port": 99999,
            "syncIntervalSecs": 5,
            "profile": "../evil",
        });
        apply_server_overrides(&mut local, &server, &[]);
        assert_eq!(local.port, Some(3180), "非法端口被忽略");
        assert_eq!(local.sync_interval_secs, Some(300), "过小同步间隔被忽略");
        assert_eq!(local.profile.as_deref(), Some("web"), "非法 profile 被忽略");
    }

    #[test]
    fn node_version_compatible_relaxes_to_major() {
        // 期望 v22：主版本 >= 22 即可（系统更新的 node 直接复用）
        assert!(node_version_compatible("v22.22.0"));
        assert!(node_version_compatible("v24.19.0"), "系统 v24 应视为可用");
        assert!(node_version_compatible("v23.0.0"));
        assert!(node_version_compatible("v22.0.0"));
        // 低于期望主版本 → 不可用（需下载自带）
        assert!(!node_version_compatible("v20.11.0"));
        assert!(!node_version_compatible("v18.0.0"));
        // 非法输入 → 不可用
        assert!(!node_version_compatible(""));
        assert!(!node_version_compatible("not-a-version"));
    }
}
