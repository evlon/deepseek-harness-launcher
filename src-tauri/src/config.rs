//! 启动器配置、路径与加速源解析。
//!
//! 配置存于 `<app_data>/launcher-config.json`（用户可编辑，重启生效）。全部字段可选，
//! 缺省按地域自动选择下载/加速源。与现有桌面端隔离：依赖装在 `<app_data>` 内，
//! Harness 用户数据（`$DSH_HOME`）默认 `~/.dsh-launcher`，互不影响。

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuickLink {
    pub label: String,
    pub url: String,
}

/// 启动器配置（JSON，全字段可选）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LauncherConfig {
    /// Harness 服务端口（缺省 3180，避开桌面端 3080）
    pub port: Option<u16>,
    /// npm registry 显式地址（空=按地域自动）。可在托盘「加速」预设中写入，
    /// 或直接手编本文件（自定义内网 Verdaccio）。
    pub npm_registry: Option<String>,
    /// GitHub 中转前缀（空=按地域自动；none 用空串表示直连）。
    pub gh_mirror_prefix: Option<String>,
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
}

/// 管理能力（外网代理网关）配置。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
pub fn detect_region() -> Region {
    static REGION: once_cell::sync::OnceCell<Region> = once_cell::sync::OnceCell::new();
    *REGION.get_or_init(|| {
        let locale = current_locale();
        let locale_zh = locale_is_china(&locale);
        let tz_china = is_china_timezone();
        let region = region_for(locale_zh, tz_china);
        log::info!(
            "Download region detected: {:?} (locale={locale:?}, china_timezone={tz_china})",
            region
        );
        region
    })
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

/// 解析实际生效的 npm registry（末尾带 `/`）。
pub fn resolve_npm_registry(cfg: &LauncherConfig) -> String {
    if let Some(reg) = &cfg.npm_registry {
        let reg = reg.trim();
        if !reg.is_empty() {
            return ensure_trailing_slash(reg);
        }
    }
    match detect_region() {
        Region::Domestic => NPM_MIRROR_REGISTRY.to_string(),
        Region::Overseas => OFFICIAL_NPM_REGISTRY.to_string(),
    }
}

/// 解析实际生效的 GitHub 中转前缀；直连返回 `None`。
///
/// `"none"`（不区分大小写）按配置文档语义视为「直连」，
/// 防止手编配置文件写入 `"ghMirrorPrefix": "none"` 时被当成字面前缀。
pub fn resolve_gh_prefix(cfg: &LauncherConfig) -> Option<String> {
    if let Some(prefix) = &cfg.gh_mirror_prefix {
        let prefix = prefix.trim();
        if prefix.is_empty() || prefix.eq_ignore_ascii_case("none") {
            return None;
        }
        return Some(ensure_trailing_slash(prefix));
    }
    match detect_region() {
        Region::Domestic => Some(DSH_MIRROR_PREFIX.to_string()),
        Region::Overseas => None,
    }
}

/// 为任意 GitHub 资产 URL 生成中转兜底地址（透传原 URL，内容一致）。
pub fn mirror_url(asset_url: &str, cfg: &LauncherConfig) -> String {
    let prefix = resolve_gh_prefix(cfg).unwrap_or_else(|| DSH_MIRROR_PREFIX.to_string());
    format!("{prefix}{asset_url}")
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

/// 启动时加载配置（文件缺失/解析失败按默认处理，不阻断启动）。
pub fn load_config<R: Runtime>(app: &AppHandle<R>) -> LauncherConfig {
    let path = config_path(app);
    let cfg = match fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
            log::warn!("launcher-config.json 解析失败，回退默认：{e}");
            LauncherConfig::default()
        }),
        Err(_) => LauncherConfig::default(),
    };
    log::info!(
        "Launcher config loaded: port={:?}, npm_registry={:?}, gh_prefix={:?}, dsh_home={:?}",
        cfg.port, cfg.npm_registry, cfg.gh_mirror_prefix, cfg.dsh_home
    );
    cache(cfg.clone());
    cfg
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

/// 设置 npm 源预设（空串=auto），写回配置。
pub fn set_npm_registry<R: Runtime>(app: &AppHandle<R>, registry: &str) -> Result<(), String> {
    let mut cfg = load_cached();
    cfg.npm_registry = if registry.is_empty() {
        None
    } else {
        Some(registry.to_string())
    };
    save_config(app, &cfg)
}

/// 设置 GitHub 中转预设（None=auto/直连），写回配置。
pub fn set_gh_prefix<R: Runtime>(app: &AppHandle<R>, prefix: Option<&str>) -> Result<(), String> {
    let mut cfg = load_cached();
    cfg.gh_mirror_prefix = match prefix {
        Some(p) if !p.is_empty() => Some(p.to_string()),
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
            npm_registry: Some("https://registry.example.com".to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_npm_registry(&cfg),
            "https://registry.example.com/"
        );
        // 空串视为 auto
        let auto = LauncherConfig {
            npm_registry: Some(String::new()),
            ..Default::default()
        };
        let _ = resolve_npm_registry(&auto); // 不 panic 即可
    }

    #[test]
    fn gh_prefix_none_means_direct() {
        // 手编配置写 "none"（任意大小写）= 直连
        let cfg = LauncherConfig {
            gh_mirror_prefix: Some("none".to_string()),
            ..Default::default()
        };
        assert_eq!(resolve_gh_prefix(&cfg), None);
        let cfg = LauncherConfig {
            gh_mirror_prefix: Some("NONE".to_string()),
            ..Default::default()
        };
        assert_eq!(resolve_gh_prefix(&cfg), None);
        // 空串 = 直连（不启用中转）
        let cfg2 = LauncherConfig {
            gh_mirror_prefix: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(resolve_gh_prefix(&cfg2), None);
        // 真实前缀照常生效并补尾斜杠
        let cfg3 = LauncherConfig {
            gh_mirror_prefix: Some("https://ghfast.top".to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_gh_prefix(&cfg3),
            Some("https://ghfast.top/".to_string())
        );
    }

    #[test]
    fn mirror_url_prepends_prefix() {
        let cfg = LauncherConfig {
            gh_mirror_prefix: Some("https://ghfast.top/".to_string()),
            ..Default::default()
        };
        assert_eq!(
            mirror_url("https://github.com/a/b.zip", &cfg),
            "https://ghfast.top/https://github.com/a/b.zip"
        );
        // 无配置时兜底 ghfast.top
        let auto = LauncherConfig::default();
        let out = mirror_url("https://github.com/a/b.zip", &auto);
        assert!(out.starts_with("https://ghfast.top/"), "got {out}");
    }

    #[test]
    fn ensure_trailing_slash_normalizes() {
        assert_eq!(ensure_trailing_slash("https://x.com"), "https://x.com/");
        assert_eq!(ensure_trailing_slash("https://x.com/"), "https://x.com/");
    }
}
