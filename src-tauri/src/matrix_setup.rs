//! 数字分身配置向导（matrix-setup）：把 dsh-matrix-agent 的 Matrix 连接配置
//! （homeserverUrl / userId / accessToken / owner）写入它与设置 UI 共用的同一配置源
//! `<DSH_HOME>/settings.yaml` 的 `dsh-matrix` section，并以分步进度引导小白完成
//! 「配置 → 重启 → 连接验证 → 可用」全流程。
//!
//! 设计要点（见 docs/matrix-setup-wizard.md）：
//! - 与 dsh-matrix-agent 设置 UI 等效：写 settings.yaml 的 dsh-matrix section；
//! - **逐字段 merge**：该 section 已被 dsh 写入 timelineSnapshot / tasksSnapshot /
//!   ownerInbox 等运行时镜像数据，写入账号键时绝不能整节覆盖或丢其它键；
//! - accessToken 可选「账号+密码自动获取」（POST {hs}/_matrix/client/v3/login），
//!   密码不落盘，token 落 settings.yaml（与现状一致）；
//! - accessToken 属 settings.ts RESTART_KEYS，写配置后须重启 matrix profile 生效。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Runtime};

use crate::config::*;

/// 最近一次「自动激活」的失败信息（供向导前端轮询感知失败并恢复按钮）。
///
/// 背景：/activate 在后台 spawn_blocking 跑，HTTP 立即返回 ok:true，前端靠轮询
/// /state 等 status=configured。失败时 status 永不变成 configured，前端只能静默
/// 超时退出——按钮永远停在 disabled。这里记录失败文案，/state 带出，前端据此
/// 显示错误 + 恢复按钮。
static LAST_ACTIVATION_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// dsh-matrix-agent 的 settings namespace（与 @evlon/dsh-bridge settings.ts MATRIX_NS 一致）。
pub const MATRIX_NS: &str = "dsh-matrix";
/// matrix profile 名（与 install.rs MATRIX_PROFILE 一致）。
pub const MATRIX_PROFILE: &str = "matrix";
/// dsh-matrix-agent 的 npm 包目录名（bundle 层 patch 预置值来源）。
pub const MATRIX_AGENT_BUNDLE: &str = "dsh-matrix-agent";
/// 占位 token（install.rs 补缺 patch 时写入，视为未配置）。
pub const PENDING_CONFIG: &str = "pending-config";

/// dsh-matrix-agent 的连接账号字段（settings schema 顶层字段）。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MatrixAccount {
    pub homeserver_url: String,
    pub user_id: String,
    pub access_token: String,
    pub owner: String,
}

impl MatrixAccount {
    /// 三要素是否齐全（homeserverUrl + userId + accessToken，非空且非占位）。
    pub fn complete(&self) -> bool {
        !self.homeserver_url.trim().is_empty()
            && !self.user_id.trim().is_empty()
            && token_ready(&self.access_token)
    }

    /// 缺失字段名列表（小白提示用）。
    pub fn missing(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.homeserver_url.trim().is_empty() {
            out.push("homeserverUrl");
        }
        if self.user_id.trim().is_empty() {
            out.push("userId");
        }
        if !token_ready(&self.access_token) {
            out.push("accessToken");
        }
        out
    }
}

/// token 是否「可用」：非空且非占位；access_token 空时允许环境变量 DSH_MATRIX_TOKEN 兜底
/// （dsh-matrix-agent 的 config 也支持 accessToken 空 → 回退 process.env.DSH_MATRIX_TOKEN）。
fn token_ready(token: &str) -> bool {
    if !token.trim().is_empty() && token.trim() != PENDING_CONFIG {
        return true;
    }
    // 空/占位时：环境变量兜底（与 dsh-matrix-agent 行为一致）
    std::env::var("DSH_MATRIX_TOKEN").map(|v| !v.trim().is_empty()).unwrap_or(false)
}

/// 向导状态。
#[derive(Debug, Clone, PartialEq)]
pub enum MatrixStatus {
    /// dsh-matrix-agent 未安装（matrix profile 未预置）——先装。
    NotInstalled,
    /// 未配置：缺哪些字段。
    Unconfigured { missing: Vec<String> },
    /// 已配置（三要素齐全）。
    Configured,
}

/// settings.yaml 路径：`<DSH_HOME>/settings.yaml`。
pub fn settings_yaml_path<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> PathBuf {
    dsh_home(app, cfg).join("settings.yaml")
}

/// 读取当前账号配置：settings.yaml 用户层优先；为空时回退 bundle/profile 层 patch 预置值。
/// 返回 (当前实际账号配置, 预置来源是否命中)。
pub fn load_account<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> (MatrixAccount, bool) {
    let path = settings_yaml_path(app, cfg);
    let user = read_ns_from_file(&path, MATRIX_NS);
    // settings.yaml 有账号值 → 直接用（用户层优先）
    if let Some(acc) = user {
        if !acc.homeserver_url.is_empty() || !acc.user_id.is_empty() || !acc.access_token.is_empty() {
            return (acc, true);
        }
    }
    // 回退：从 matrix profile 的 bundle 层 patch 读预置（homeserverUrl/userId/owner）
    let preset = preset_from_bundle_patch(app, cfg);
    (preset, false)
}

/// 当前向导状态。
pub fn status<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> MatrixStatus {
    if !matrix_agent_installed(app, cfg) {
        return MatrixStatus::NotInstalled;
    }
    let (acc, _) = load_account(app, cfg);
    if acc.complete() {
        MatrixStatus::Configured
    } else {
        MatrixStatus::Unconfigured {
            missing: acc.missing().iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// dsh-matrix-agent 是否已装进 matrix profile（node_modules 存在 bundle patch）。
pub fn matrix_agent_installed<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> bool {
    dsh_home(app, cfg)
        .join("profiles")
        .join(MATRIX_PROFILE)
        .join("node_modules")
        .join(MATRIX_AGENT_BUNDLE)
        .join("cordis.patch.yml")
        .exists()
}

/// 从 dsh-matrix-agent 的 bundle 层 patch（cordis.patch.yml）读取预置账号字段。
/// bundle patch 顶层是 insert 数组：`- insert: - id: <注册名> name: dsh-matrix-agent config: {...}`。
fn preset_from_bundle_patch<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> MatrixAccount {
    let patch_path = dsh_home(app, cfg)
        .join("profiles")
        .join(MATRIX_PROFILE)
        .join("node_modules")
        .join(MATRIX_AGENT_BUNDLE)
        .join("cordis.patch.yml");
    let Ok(text) = std::fs::read_to_string(&patch_path) else {
        return MatrixAccount::default();
    };
    parse_preset_patch(&text)
}

/// 解析 bundle patch 文本 → 预置账号（config 段取 homeserverUrl/userId/accessToken/owner）。
/// 供单测直接测（纯函数）。
fn parse_preset_patch(text: &str) -> MatrixAccount {
    use serde_yaml::Value;
    let Ok(root) = serde_yaml::from_str::<Value>(text) else {
        return MatrixAccount::default();
    };
    let Some(entries) = root.as_sequence() else {
        return MatrixAccount::default();
    };
    let mut acc = MatrixAccount::default();
    for entry in entries {
        // 顶层 entry: { insert: [ { id/name/config }, ... ] }
        if let Some(insert) = entry.get("insert").and_then(|v| v.as_sequence()) {
            for item in insert {
                // item 可能是 { id: ..., name: dsh-matrix-agent, config: {...} }
                let is_target = item.get("name").and_then(|v| v.as_str()) == Some(MATRIX_AGENT_BUNDLE)
                    || item.get("id").and_then(|v| v.as_str()) == Some("matrix");
                if !is_target {
                    continue;
                }
                let Some(cfg) = item.get("config").and_then(|v| v.as_mapping()) else {
                    continue;
                };
                let s = |k: &str| -> String {
                    cfg.get(Value::String(k.to_string()))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                };
                acc.homeserver_url = s("homeserverUrl");
                acc.user_id = s("userId");
                acc.access_token = s("accessToken");
                acc.owner = s("owner");
            }
        }
    }
    acc
}

/// 从 YAML 文件读某个 namespace section，反序列化为 MatrixAccount（仅取已知键，忽略其它）。
/// 文件不存在 / 解析失败 / section 缺失 → None。
fn read_ns_from_file(path: &Path, ns: &str) -> Option<MatrixAccount> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_ns(&text, ns)
}

/// 从 YAML 文本读 namespace section（纯函数，单测用）。
/// 只覆盖 MatrixAccount 已知键；section 下其它键（timelineSnapshot 等）保持忽略。
fn parse_ns(text: &str, ns: &str) -> Option<MatrixAccount> {
    use serde_yaml::Value;
    let root: Value = serde_yaml::from_str(text).ok()?;
    let section = root.get(ns)?;
    let mapping = section.as_mapping()?;
    let s = |k: &str| -> String {
        mapping
            .get(Value::String(k.to_string()))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    Some(MatrixAccount {
        homeserver_url: s("homeserverUrl"),
        user_id: s("userId"),
        access_token: s("accessToken"),
        owner: s("owner"),
    })
}

/// 写账号配置：解析整个 settings.yaml → 覆盖 dsh-matrix section 的账号键（逐字段 merge，
/// 保留 timelineSnapshot 等其它键）→ 原子写回。文件不存在则新建。
pub fn write_account<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig, acc: &MatrixAccount) -> Result<(), String> {
    let path = settings_yaml_path(app, cfg);
    write_account_to_file(&path, acc).map_err(|e| e)
}

/// 写账号配置到指定文件（纯逻辑，单测用；tmp+rename 原子写）。
fn write_account_to_file(path: &Path, acc: &MatrixAccount) -> Result<(), String> {
    use serde_yaml::{Mapping, Value};
    let mut root: Mapping = match std::fs::read_to_string(path) {
        Ok(text) => {
            let v: Value = serde_yaml::from_str(&text)
                .map_err(|e| format!("SETTINGS_PARSE_FAILED: {e}"))?;
            v.as_mapping().cloned().unwrap_or_default()
        }
        Err(_) => Mapping::new(), // 文件不存在 → 新建
    };
    // 取或建 dsh-matrix section
    let section = root
        .entry(Value::String(MATRIX_NS.to_string()))
        .or_insert_with(|| Value::Mapping(Mapping::new()));
    let section_map = section
        .as_mapping_mut()
        .ok_or("SETTINGS_NS_NOT_MAP: dsh-matrix 不是 map")?;
    // 只覆盖账号键（其它键如 timelineSnapshot 原样保留）
    let set = |m: &mut Mapping, k: &str, v: &str| {
        m.insert(Value::String(k.to_string()), Value::String(v.to_string()));
    };
    set(section_map, "homeserverUrl", &acc.homeserver_url);
    set(section_map, "userId", &acc.user_id);
    set(section_map, "accessToken", &acc.access_token);
    if !acc.owner.is_empty() {
        set(section_map, "owner", &acc.owner);
    }
    let out = serde_yaml::to_string(&Value::Mapping(root))
        .map_err(|e| format!("SETTINGS_SERIALIZE_FAILED: {e}"))?;
    // 原子写：tmp + rename（防 dsh 进程并发读到半截文件）
    atomic_write(path, out.as_bytes())
}

/// dsh-himarket 的 settings namespace（与 dsh-himarket/src/settings.ts NAMESPACE 一致）。
pub const HIMARKET_NS: &str = "himarket";

/// HiMarket 一键登录（SSO）结果：写入 settings.yaml `himarket` section 的字段。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HimarketLogin {
    /// HiMarket 门户地址（如 http://market.ai.ict.cmcc）
    pub base_url: String,
    /// Keycloak preferred_username（展示用；SSO 登录时非密码账号）
    pub username: String,
    /// 员工中文姓名（Keycloak `name` claim）；realm 未配 mapper 时为空。
    ///
    /// ⚠️ 只作**展示**用（管理页「谁在用这台机器」），不参与任何鉴权判定。
    pub display_name: String,
    /// 开发者 JWT（7 天有效，来自 /developers/oauth2/token）
    pub token: String,
}

/// 写 HiMarket 登录态：只覆盖 `himarket` section 的 baseUrl/username/token，
/// **保留** gatewayUrl / adminUsername / portalId / skillInstallDir 等既有键
/// （与 write_account 同样的逐字段 merge 语义）。
///
/// ⚠️ 为什么不写 password：SSO 登录没有密码；写入空串会覆盖用户手工兜底配置，
/// 故仅在调用方明确给出非空 password 时才写（本函数不接收 password）。
pub fn write_himarket_login<R: Runtime>(
    app: &AppHandle<R>,
    cfg: &LauncherConfig,
    login: &HimarketLogin,
) -> Result<(), String> {
    let path = settings_yaml_path(app, cfg);
    write_himarket_login_to_file(&path, login)
}

/// 写 HiMarket 登录态到指定文件（纯逻辑，单测用；tmp+rename 原子写）。
fn write_himarket_login_to_file(path: &Path, login: &HimarketLogin) -> Result<(), String> {
    use serde_yaml::{Mapping, Value};
    let mut root: Mapping = match std::fs::read_to_string(path) {
        Ok(text) => {
            let v: Value = serde_yaml::from_str(&text)
                .map_err(|e| format!("SETTINGS_PARSE_FAILED: {e}"))?;
            v.as_mapping().cloned().unwrap_or_default()
        }
        Err(_) => Mapping::new(),
    };
    let section = root
        .entry(Value::String(HIMARKET_NS.to_string()))
        .or_insert_with(|| Value::Mapping(Mapping::new()));
    let m = section
        .as_mapping_mut()
        .ok_or("SETTINGS_NS_NOT_MAP: himarket 不是 map")?;
    let set = |m: &mut Mapping, k: &str, v: &str| {
        if !v.is_empty() {
            m.insert(Value::String(k.to_string()), Value::String(v.to_string()));
        }
    };
    // 只写非空值：SSO 登录缺 username 时不应把用户已填的兜底账号清掉
    set(m, "baseUrl", &login.base_url);
    set(m, "username", &login.username);
    set(m, "token", &login.token);
    // 员工姓名（管理页展示「谁在用这台机器」）。独立键 displayName，
    // 不与 username 混用：username 是登录账号（niukunliang），displayName 是中文姓名（牛昆亮）。
    set(m, "displayName", &login.display_name);
    let out = serde_yaml::to_string(&Value::Mapping(root))
        .map_err(|e| format!("SETTINGS_SERIALIZE_FAILED: {e}"))?;
    atomic_write(path, out.as_bytes())
}

/// HiMarket 登录态（托盘菜单展示用）。
#[derive(Debug, Clone, PartialEq)]
pub enum HimarketTokenState {
    /// 已登录：携带展示用用户名（可为空）与员工姓名（可为空）。
    LoggedIn { username: String, display_name: String },
    /// 未登录（token 为空/缺失）。
    NotLoggedIn,
}

/// 员工身份（上报给中心管理页「谁在用这台机器」）。
///
/// 来源三处，按可信度排序：
///   1. `himarket.username` / `himarket.displayName` —— SSO 登录写入（Keycloak 权威）
///   2. `dsh-matrix.owner` —— 数字分身的主人 Matrix userId（如 `@niukunliang:im.ai.ict.cmcc`）
///   3. `dsh-matrix.userId` —— 分身自身 userId（如 `@ai-niukunliang:...`）
///
/// ⚠️ 纯展示字段，**不参与鉴权**。管理页只读它来回答「这台机器是谁的」。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientIdentity {
    /// 登录账号（Keycloak preferred_username，如 niukunliang）
    pub username: String,
    /// 员工中文姓名（Keycloak name，如 牛昆亮）；未配 claim 时为空
    pub display_name: String,
    /// 分身主人的 Matrix userId（如 @niukunliang:im.ai.ict.cmcc）
    pub owner: String,
    /// 数字分身自身 Matrix userId（如 @ai-niukunliang:im.ai.ict.cmcc）
    pub twin_user_id: String,
}

impl ClientIdentity {
    /// 是否拿到任何可用身份信息（全空则管理页显示「未登录」）。
    pub fn any(&self) -> bool {
        !self.username.trim().is_empty()
            || !self.display_name.trim().is_empty()
            || !self.owner.trim().is_empty()
            || !self.twin_user_id.trim().is_empty()
    }
}

/// 读本机员工身份（纯文件读取，失败返回空身份，绝不阻断上报）。
pub fn read_identity<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> ClientIdentity {
    let path = settings_yaml_path(app, cfg);
    read_identity_from_file(&path)
}

/// 从 settings.yaml 读员工身份（纯逻辑，单测用）。
pub fn read_identity_from_file(path: &Path) -> ClientIdentity {
    use serde_yaml::Value;
    let mut out = ClientIdentity::default();
    let Ok(text) = std::fs::read_to_string(path) else {
        return out;
    };
    let Ok(root) = serde_yaml::from_str::<Value>(&text) else {
        return out;
    };
    let get = |ns: &str, k: &str| -> String {
        root.get(ns)
            .and_then(|s| s.as_mapping())
            .and_then(|m| m.get(Value::String(k.to_string())))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    out.username = get(HIMARKET_NS, "username");
    out.display_name = get(HIMARKET_NS, "displayName");
    out.owner = get(MATRIX_NS, "owner");
    out.twin_user_id = get(MATRIX_NS, "userId");
    // 兜底：SSO 未登录（无 username）但分身已配置时，从 owner userId 反推账号名。
    // 形如 `@niukunliang:im.ai.ict.cmcc` → `niukunliang`；分身自身 userId 带 `ai-` 前缀，
    // 那是机器账号不是人，故只在 owner 上做这个反推。
    if out.username.is_empty() {
        out.username = localpart_of(&out.owner);
    }
    out
}

/// 从 Matrix userId（`@name:server`）取 localpart（`name`）。
/// 非该形状（空串 / 缺 `@` 或 `:`）返回空串。
fn localpart_of(user_id: &str) -> String {
    let t = user_id.trim();
    let Some(rest) = t.strip_prefix('@') else {
        return String::new();
    };
    match rest.split_once(':') {
        Some((name, _)) if !name.is_empty() => name.to_string(),
        _ => String::new(),
    }
}

/// 读 HiMarket 登录态：读 settings.yaml 的 `himarket.token`。
/// token 非空即视为已登录（不校验过期——过期由插件 401 时重登处理）。
pub fn himarket_token_state<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> HimarketTokenState {
    let path = settings_yaml_path(app, cfg);
    himarket_token_state_in_file(&path)
}

/// 读 HiMarket 登录态（纯逻辑，单测用）。
fn himarket_token_state_in_file(path: &Path) -> HimarketTokenState {
    use serde_yaml::Value;
    let Ok(text) = std::fs::read_to_string(path) else {
        return HimarketTokenState::NotLoggedIn;
    };
    let Ok(root) = serde_yaml::from_str::<Value>(&text) else {
        return HimarketTokenState::NotLoggedIn;
    };
    let Some(section) = root.get(HIMARKET_NS).and_then(|s| s.as_mapping()) else {
        return HimarketTokenState::NotLoggedIn;
    };
    let token = section
        .get(Value::String("token".to_string()))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if token.trim().is_empty() {
        return HimarketTokenState::NotLoggedIn;
    }
    let s = |k: &str| -> String {
        section
            .get(Value::String(k.to_string()))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    HimarketTokenState::LoggedIn {
        username: s("username"),
        display_name: s("displayName"),
    }
}

/// 清理 HiMarket 登录态：token/username 置空（保留 baseUrl 与其它键）。
pub fn clear_himarket_login<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> Result<(), String> {
    let path = settings_yaml_path(app, cfg);
    clear_himarket_login_in_file(&path)
}

/// 清理 HiMarket 登录态（纯逻辑，单测用）。
fn clear_himarket_login_in_file(path: &Path) -> Result<(), String> {
    use serde_yaml::Value;
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(());
    };
    let root: Value = serde_yaml::from_str(&text)
        .map_err(|e| format!("SETTINGS_PARSE_FAILED: {e}"))?;
    let Some(mut mapping) = root.as_mapping().cloned() else {
        return Ok(());
    };
    if let Some(section) = mapping.get_mut(Value::String(HIMARKET_NS.to_string())) {
        if let Some(m) = section.as_mapping_mut() {
            for k in ["token", "username", "password"] {
                m.insert(Value::String(k.to_string()), Value::String(String::new()));
            }
        }
    }
    let out = serde_yaml::to_string(&Value::Mapping(mapping))
        .map_err(|e| format!("SETTINGS_SERIALIZE_FAILED: {e}"))?;
    atomic_write(path, out.as_bytes())
}

/// 清理账号配置：账号键置空（保留镜像键），供测试反复走引导。
pub fn clear_account<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> Result<(), String> {
    let path = settings_yaml_path(app, cfg);
    clear_account_in_file(&path)
}

/// 清理账号配置（纯逻辑，单测用）：账号键置空串。
fn clear_account_in_file(path: &Path) -> Result<(), String> {
    use serde_yaml::Value;
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(()); // 文件不存在 → 无配置可清
    };
    let root: Value = serde_yaml::from_str(&text)
        .map_err(|e| format!("SETTINGS_PARSE_FAILED: {e}"))?;
    let Some(mut mapping) = root.as_mapping().cloned() else {
        return Ok(());
    };
    if let Some(section) = mapping.get_mut(Value::String(MATRIX_NS.to_string())) {
        if let Some(m) = section.as_mapping_mut() {
            for k in ["homeserverUrl", "userId", "accessToken", "owner"] {
                m.insert(Value::String(k.to_string()), Value::String(String::new()));
            }
        }
    }
    let out = serde_yaml::to_string(&Value::Mapping(mapping))
        .map_err(|e| format!("SETTINGS_SERIALIZE_FAILED: {e}"))?;
    atomic_write(path, out.as_bytes())
}

/// tmp + rename 原子写。
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    atomic_write_public(path, bytes)
}

/// tmp + rename 原子写（供 `env_defaults` 等模块复用）。
///
/// 防 dsh 进程并发读到半截文件：先写 `.yaml.tmp` 再 rename（同分区 rename 原子）。
pub fn atomic_write_public(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("MKDIR_FAILED: {e}"))?;
    }
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("TMP_WRITE_FAILED: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("RENAME_FAILED: {e}"))?;
    Ok(())
}

// ---------- 等待 Matrix 连接 ----------

/// 等待 Matrix 桥连接就绪：轮询 dsh-matrix-agent 的 diagnostics.log（stateDir），
/// 出现 "config complete" / "bridge started" 且无 "incomplete config" 即成功。
/// 超时返回错误（可重试）。
fn wait_matrix_ready<R: Runtime>(
    app: &AppHandle<R>,
    cfg: &LauncherConfig,
    timeout: std::time::Duration,
) -> Result<(), String> {
    // dsh-matrix-agent 的 stateDir 缺省 `.dsh-matrix`（相对 DSH_HOME）
    let diag_path = dsh_home(app, cfg).join(".dsh-matrix").join("diagnostics.log");
    let deadline = std::time::Instant::now() + timeout;
    // 先给启动 3s（进程 spawn + 插件加载）
    std::thread::sleep(std::time::Duration::from_secs(3));
    while std::time::Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(&diag_path) {
            // 全文判断（不做字节切片，避免切中文 panic）
            let full = text.as_str();
            // 出现成功信号：bridge started / settings register OK + 配置完整
            let started = full.contains("bridge started") || full.contains("Matrix bridge started");
            let incomplete = full.contains("incomplete config") || full.contains("not started");
            let configured = full.contains("config complete") || full.contains("starting Matrix bridge");
            if started || (configured && !incomplete) {
                log::info!("[matrix-setup] Matrix 桥已连接（诊断日志确认）");
                return Ok(());
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1500));
    }
    Err("等待 Matrix 连接超时——请查看日志确认数字分身配置是否正确".to_string())
}

// ---------- 向导窗口 ----------

use tauri::{AppHandle as TauriAppHandle, Manager, Runtime as TauriRuntime, WebviewUrl, WebviewWindowBuilder};

/// 打开（或聚焦）配置向导窗口。
pub fn open_window<R: TauriRuntime>(app: &TauriAppHandle<R>) -> Result<(), String> {
    // 打开向导前先确保 matrix profile 骨架存在（dsh 已装但未激活的路径会跳过
    // install_all，导致 profile 从未创建——见 install::ensure_matrix_profile 文档）。
    // 失败不阻断打开向导（用户仍可看到并触发激活），但记日志：激活时还会再兜底一次。
    if let Err(e) = crate::install::ensure_matrix_profile(app, &load_cached()) {
        log::warn!("打开激活向导前确保 matrix profile 失败（激活时兜底重试）：{e}");
    }
    if let Some(win) = app.get_webview_window("matrix-setup") {
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(());
    }
    let url = WebviewUrl::External(
        "http://matrix-setup.localhost/index.html"
            .parse()
            .map_err(|e: url::ParseError| e.to_string())?,
    );
    WebviewWindowBuilder::new(app, "matrix-setup", url)
        .title("配置数字分身")
        .inner_size(500.0, 560.0)
        .resizable(true)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// matrix-setup scheme 协议处理：GET / → HTML；GET /state → JSON；POST /fetch-token、/submit。
pub fn handle_scheme_request<R: TauriRuntime>(
    ctx: &tauri::UriSchemeContext<'_, R>,
    request: tauri::http::Request<Vec<u8>>,
) -> tauri::http::Response<Vec<u8>> {
    use tauri::http::{header, Response, StatusCode};
    let app = ctx.app_handle();
    let path = request.uri().path().to_string();
    let method = request.method().clone();
    log::info!("matrix-setup:// 协议请求：{method} {path}");

    // CORS/通用 JSON 辅助
    let json_resp = |status: StatusCode, obj: serde_json::Value| -> Response<Vec<u8>> {
        Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
            .body(serde_json::to_string(&obj).unwrap_or_else(|_| "{}".into()).into_bytes())
            .unwrap_or_default()
    };
    let cfg = load_cached();

    if method == tauri::http::Method::GET && (path == "/" || path == "/index.html") {
        let html = wizard_html();
        return Response::builder()
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(
                "Content-Security-Policy",
                "default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self' http://matrix-setup.localhost",
            )
            .body(html.into_bytes())
            .unwrap_or_default();
    }
    if method == tauri::http::Method::GET && path == "/state" {
        let st = collect_state(app, &cfg);
        return json_resp(StatusCode::OK, serde_json::to_value(&st).unwrap_or_else(|_| serde_json::json!({})));
    }
    if method == tauri::http::Method::POST && path == "/activate" {
        // 自动激活：授权码 + PKCE + 本地回调（P3）。后台线程跑完整流程
        // （起回调 → 打开浏览器 → 等回调 → 换 token → 调 /activate → 写配置 → 重启）。
        // 与 /submit 一致：任务移交后台，进度走 ops + console 窗口，向导窗口关闭。
        let h = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            crate::ops::start_op(&h, "matrix-activate", "自动激活数字分身", &["浏览器授权", "认领身份", "写入配置", "重启数字分身", "等待连接"]);
            crate::ops::mark_step_running(&h, 0);
            crate::ops::update_step(&h, "等待浏览器授权…");
            crate::ops::append_log(&h, "已打开浏览器，请在浏览器中完成公司 SSO 登录…");
            match crate::activation::run_activation(&h) {
                r if r.ok => {
                    crate::ops::mark_step_running(&h, 2);
                    crate::ops::update_step(&h, "已认领分身，写入配置…");
                    crate::ops::append_log(&h, &format!("✓ 已认领分身账号 {}", r.user_id));
                    // 配置已由 run_activation 写入 settings.yaml，这里需确保 profile 骨架存在
                    // 后再重启（dsh 启动 --profile matrix 要求 manifest 已建，否则报
                    // "profile does not exist" → HARNESS_NOT_READY）。
                    let cfg_now = load_cached();
                    if let Err(e) = crate::install::ensure_matrix_profile(&h, &cfg_now) {
                        crate::ops::append_log(&h, &format!("⚠️ 确保数字分身运行环境失败：{e}"));
                    }
                    let running = crate::workflow::is_running();
                    let cur_profile = crate::workflow::current_profile();
                    crate::ops::mark_step_running(&h, 3);
                    crate::ops::update_step(&h, "重启数字分身…");
                    if running && cur_profile.as_deref() == Some(MATRIX_PROFILE) {
                        crate::workflow::stop();
                        crate::ops::append_log(&h, "已停止旧数字分身进程（连接参数需重启生效）");
                        std::thread::sleep(std::time::Duration::from_millis(800));
                    }
                    match crate::workflow::launch_with_profile(&h, MATRIX_PROFILE) {
                        Ok(port) => {
                            // 启动成功：清空「最近激活失败」标记（前端轮询感知到 configured 即收尾）
                            *LAST_ACTIVATION_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = None;
                            crate::ops::append_log(&h, &format!("✓ 数字分身已启动：{}", crate::workflow::access_url(port)));
                            crate::ops::mark_step_running(&h, 4);
                            crate::ops::update_step(&h, "等待 Matrix 连接…");
                            match wait_matrix_ready(&h, &cfg_now, std::time::Duration::from_secs(45)) {
                                Ok(()) => {
                                    crate::ops::finish_op(&h, &format!("数字分身已激活并可用：{}", r.user_id));
                                    crate::notify::notify(&h, "数字分身已激活", &format!("{} 已就绪，可在 Matrix 客户端 @ 它试试", r.user_id));
                                    crate::tray::refresh_sync_menu(&h);
                                }
                                Err(e) => {
                                    crate::ops::finish_op(&h, &format!("数字分身已激活（{}），但连接等待超时：{}", r.user_id, e));
                                    crate::notify::notify(&h, "数字分身已激活", &format!("{} 已写入配置。连接验证超时（不影响使用），可稍后在托盘查看。", r.user_id));
                                }
                            }
                        }
                        Err(e) => {
                            crate::ops::fail_op(&h, &format!("分身已激活但启动失败：{e}"));
                            crate::notify::notify(&h, "数字分身已激活", &format!("{} 已写入配置，但启动失败：{}。可在托盘「启动」重试。", r.user_id, e));
                            // 记录失败文案，供向导前端轮询感知 + 恢复按钮（避免「失败后按钮锁死」）
                            *LAST_ACTIVATION_ERROR.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(format!("分身已激活但启动失败：{e}"));
                        }
                    }
                }
                r => {
                    crate::ops::fail_op(&h, &r.message);
                    crate::notify::notify(&h, "自动激活失败", &r.message);
                    // 记录失败文案，供向导前端轮询感知 + 恢复按钮
                    *LAST_ACTIVATION_ERROR.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(r.message.clone());
                }
            }
        });
        // 弹进度窗口；向导窗口是否关闭取决于「是否还有岗位待选」：
        // - 服务端下发了岗位候选（jobPresets 非空）→ 保留向导窗口，切到「选岗位」步骤；
        // - 无岗位候选 → 照旧关闭向导（进度走操作窗口）。
        let has_jobs = !job_presets_from_sync(app, &cfg).is_empty();
        let h = app.clone();
        tauri::async_runtime::spawn(async move {
            std::thread::sleep(std::time::Duration::from_millis(400));
            let _ = crate::console::open_console(&h);
            if !has_jobs {
                std::thread::sleep(std::time::Duration::from_millis(300));
                if let Some(win) = h.get_webview_window("matrix-setup") {
                    let _ = win.close();
                }
            }
        });
        return json_resp(StatusCode::OK, serde_json::json!({"ok": true, "message": "已开始自动激活"}));
    }
    if method == tauri::http::Method::POST && path == "/jobs" {
        // 「选岗位」提交：用户勾选预装岗位 + 选默认岗位后落盘。
        // body: { jobs: ["pm","dev"], defaultJob: "pm" }（jobs 可为空数组，defaultJob 可为空串）。
        let body: serde_json::Value = match serde_json::from_slice(request.body()) {
            Ok(v) => v,
            Err(_) => {
                return json_resp(
                    StatusCode::OK,
                    serde_json::json!({"ok": false, "error": "请求体不是合法 JSON"}),
                )
            }
        };
        let jobs: Vec<String> = body
            .get("jobs")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let default_job: String = body
            .get("defaultJob")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        // 校验 defaultJob 若给值，必须属于已勾选的 jobs（或至少是合法岗位 id）
        if !default_job.is_empty() && !jobs.iter().any(|j| j == &default_job) {
            return json_resp(
                StatusCode::OK,
                serde_json::json!({"ok": false, "error": "默认岗位必须从已勾选的岗位里选"}),
            );
        }
        let path = settings_yaml_path(app, &cfg);
        // 落盘预装岗位清单（用户勾选的）+ 默认岗位
        let jobs_result = crate::env_defaults::apply_job_presets_to_file(&path, &jobs);
        let dj_result = crate::env_defaults::apply_default_job_to_file(&path, &default_job);
        if let Err(e) = jobs_result {
            log::warn!("[jobs] 落盘预装岗位清单失败：{e}");
        }
        if let Err(e) = dj_result {
            log::warn!("[jobs] 落盘默认岗位失败：{e}");
        }
        log::info!(
            "[jobs] 已保存岗位设置：预装 {} 个，默认岗位 {}",
            jobs.len(),
            if default_job.is_empty() { "（未指定）" } else { &default_job }
        );
        // 关闭向导窗口
        if let Some(win) = app.get_webview_window("matrix-setup") {
            let _ = win.close();
        }
        return json_resp(
            StatusCode::OK,
            serde_json::json!({"ok": true, "message": "岗位设置已保存"}),
        );
    }
    if method == tauri::http::Method::POST && path == "/submit" {
        // 手动配置提交：默认禁用（连接参数由服务端下发 + 自动激活写入），
        // 仅当开发者本地把 launcher-config.json 的 matrixManualConfig 改为 true 时启用，
        // 便于和测试环境（自建 homeserver）联调。
        if !manual_config_enabled(&cfg) {
            return json_resp(
                StatusCode::OK,
                serde_json::json!({"ok": false, "error": "手动配置已禁用。请使用上方「自动激活数字分身」，连接参数由服务端统一下发。"}),
            );
        }
        // 读取请求体：{ homeserverUrl, userId, accessToken, owner }
        let body: serde_json::Value = match serde_json::from_slice(request.body()) {
            Ok(v) => v,
            Err(_) => {
                return json_resp(
                    StatusCode::OK,
                    serde_json::json!({"ok": false, "error": "请求体不是合法 JSON"}),
                )
            }
        };
        let s = |k: &str| -> String {
            body.get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let acc = MatrixAccount {
            homeserver_url: s("homeserverUrl"),
            user_id: s("userId"),
            access_token: s("accessToken"),
            owner: s("owner"),
        };
        // 三要素校验（owner 可选）
        if !acc.complete() {
            return json_resp(
                StatusCode::OK,
                serde_json::json!({"ok": false, "error": "服务器地址、分身账号、访问令牌三项必填" }),
            );
        }
        // 写配置 + 重启 matrix profile 生效（与自动激活一致）
        let h = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            crate::ops::start_op(&h, "matrix-manual", "手动配置数字分身", &["写入配置", "重启数字分身", "等待连接"]);
            crate::ops::mark_step_running(&h, 0);
            match write_account(&h, &load_cached(), &acc) {
                Ok(()) => {
                    crate::ops::append_log(&h, "✓ 已写入连接配置");
                    let cfg_now = load_cached();
                    // 手动配置同样可能发生在「dsh 已装但 profile 未建」的路径，启动前兜底建骨架
                    if let Err(e) = crate::install::ensure_matrix_profile(&h, &cfg_now) {
                        crate::ops::append_log(&h, &format!("⚠️ 确保数字分身运行环境失败：{e}"));
                    }
                    let running = crate::workflow::is_running();
                    let cur_profile = crate::workflow::current_profile();
                    crate::ops::mark_step_running(&h, 1);
                    if running && cur_profile.as_deref() == Some(MATRIX_PROFILE) {
                        crate::workflow::stop();
                        std::thread::sleep(std::time::Duration::from_millis(800));
                    }
                    match crate::workflow::launch_with_profile(&h, MATRIX_PROFILE) {
                        Ok(port) => {
                            *LAST_ACTIVATION_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = None;
                            crate::ops::append_log(&h, &format!("✓ 数字分身已启动：{}", crate::workflow::access_url(port)));
                            crate::ops::mark_step_running(&h, 2);
                            match wait_matrix_ready(&h, &cfg_now, std::time::Duration::from_secs(45)) {
                                Ok(()) => {
                                    crate::ops::finish_op(&h, "数字分身已配置并连接成功");
                                    crate::notify::notify(&h, "数字分身已配置", "连接成功，可在 Matrix 客户端 @ 它试试");
                                    crate::tray::refresh_sync_menu(&h);
                                }
                                Err(e) => {
                                    crate::ops::finish_op(&h, &format!("配置已写入，但连接等待超时：{e}"));
                                    crate::notify::notify(&h, "数字分身已配置", "配置已写入，连接验证超时（不影响使用），可稍后在托盘查看。");
                                }
                            }
                        }
                        Err(e) => {
                            crate::ops::fail_op(&h, &format!("配置已写入但启动失败：{e}"));
                            crate::notify::notify(&h, "数字分身已配置", &format!("配置已写入，但启动失败：{}。可在托盘「启动」重试。", e));
                        }
                    }
                }
                Err(e) => {
                    crate::ops::fail_op(&h, &format!("写入配置失败：{e}"));
                    crate::notify::notify(&h, "手动配置失败", &e);
                }
            }
        });
        // 弹进度窗口 + 关闭向导（与 /activate 一致）
        let h = app.clone();
        tauri::async_runtime::spawn(async move {
            std::thread::sleep(std::time::Duration::from_millis(400));
            let _ = crate::console::open_console(&h);
            std::thread::sleep(std::time::Duration::from_millis(300));
            if let Some(win) = h.get_webview_window("matrix-setup") {
                let _ = win.close();
            }
        });
        return json_resp(StatusCode::OK, serde_json::json!({"ok": true, "message": "已提交手动配置"}));
    }
    json_resp(StatusCode::NOT_FOUND, serde_json::json!({"ok": false, "error": "not found"}))
}

/// 向导表单初始数据（GET /state 返回；预置值 + 当前已填值）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct WizardState {
    pub status: String,
    pub missing: Vec<String>,
    pub homeserver_url: String,
    pub user_id: String,
    pub access_token_set: bool,
    pub owner: String,
    /// 是否启用「手动配置」（开发者本地开关 matrixManualConfig，默认 false）。
    pub manual_config_enabled: bool,
    /// 订阅/安装清单差异（待装或待更新的推荐插件），激活成功后引导去装。
    pub pending_plugins: Vec<serde_json::Value>,
    /// 服务端下发的预装岗位候选清单（jobPresets），激活成功后让用户勾选预装 + 选默认岗位。
    pub job_presets: Vec<String>,
    /// 当前已落盘的默认岗位（himarket.defaultJob），空 = 未指定。
    pub default_job: String,
    /// 最近一次自动激活的失败文案（无失败/未激活过 = 空串）。前端据此恢复按钮并提示。
    pub last_activation_error: String,
}

/// 计算订阅/安装清单差异：服务端推荐清单 vs 本地已装清单。
/// 返回 [{name, installed, latest, action}]，action ∈ install | update。
fn pending_plugin_diff<R: TauriRuntime>(app: &TauriAppHandle<R>, cfg: &LauncherConfig) -> Vec<serde_json::Value> {
    let state = crate::sync::load_state(app, cfg);
    // 数字分身向导面向 matrix profile，按 matrix 的清单取（profilePlugins.matrix 优先）。
    let Some(recommended) = state
        .cached_config
        .as_ref()
        .map(|c| crate::sync::plugins_for_profile(c, MATRIX_PROFILE))
    else {
        return Vec::new();
    };
    let installed = crate::sync::installed_plugins_current_profile_with_versions(app, cfg);
    crate::sync::pending_with_updates(&recommended, &installed, &state.plugin_latest_versions)
}

/// 服务端下发的预装岗位候选清单（jobPresets）。
fn job_presets_from_sync<R: TauriRuntime>(app: &TauriAppHandle<R>, cfg: &LauncherConfig) -> Vec<String> {
    crate::sync::load_state(app, cfg)
        .cached_config
        .as_ref()
        .map(|c| c.job_presets.clone())
        .unwrap_or_default()
}

/// 读取当前 settings.yaml 里的 `himarket.defaultJob`（默认岗位），空 = 未指定。
fn read_default_job<R: TauriRuntime>(app: &TauriAppHandle<R>, cfg: &LauncherConfig) -> String {
    let path = settings_yaml_path(app, cfg);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return String::new(),
    };
    use serde_yaml::Value;
    let root: Value = match serde_yaml::from_str(&text) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    root.get("himarket")
        .and_then(|s| s.as_mapping())
        .and_then(|m| m.get(Value::String("defaultJob".to_string())))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// 收集向导初始数据（读当前配置 + 预置）。
pub fn collect_state<R: TauriRuntime>(app: &TauriAppHandle<R>, cfg: &LauncherConfig) -> WizardState {
    let st = status(app, cfg);
    let (acc, _) = load_account(app, cfg);
    let (status_str, missing) = match &st {
        MatrixStatus::Configured => ("configured".to_string(), Vec::new()),
        MatrixStatus::NotInstalled => ("not-installed".to_string(), Vec::new()),
        MatrixStatus::Unconfigured { missing } => ("unconfigured".to_string(), missing.clone()),
    };
    WizardState {
        status: status_str,
        missing,
        homeserver_url: acc.homeserver_url,
        user_id: acc.user_id,
        access_token_set: !acc.access_token.is_empty() && acc.access_token != PENDING_CONFIG,
        owner: acc.owner,
        manual_config_enabled: manual_config_enabled(&cfg),
        pending_plugins: pending_plugin_diff(app, cfg),
        job_presets: job_presets_from_sync(app, cfg),
        default_job: read_default_job(app, cfg),
        last_activation_error: LAST_ACTIVATION_ERROR
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default(),
    }
}

/// 向导 HTML（内嵌，无需前端构建；仿 console.rs）。
pub fn wizard_html() -> String {
    let html = r#"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<title>配置数字分身</title>
<style>
  :root{--bg:#1a2233;--card:#232c40;--text:#e6eaf2;--muted:#8b95a9;--green:#4ade80;--amber:#fbbf24;--red:#f87171;--blue:#60a5fa;--line:#2d3650}
  *{box-sizing:border-box;margin:0;padding:0}
  body{font-family:-apple-system,"Segoe UI",Roboto,"PingFang SC","Microsoft YaHei",sans-serif;background:var(--bg);color:var(--text);padding:18px;font-size:13px;line-height:1.6}
  h1{font-size:16px;margin-bottom:4px}
  .sub{font-size:12px;color:var(--muted);margin-bottom:14px}
  .section{background:var(--card);border:1px solid var(--line);border-radius:10px;padding:14px;margin-bottom:12px}
  .section h2{font-size:13px;margin-bottom:10px;color:var(--blue)}
  label{display:block;font-size:12px;color:var(--muted);margin:8px 0 4px}
  input{width:100%;padding:8px 10px;border:1px solid var(--line);border-radius:7px;background:#161d2e;color:var(--text);font-size:13px}
  input:focus{outline:none;border-color:var(--blue)}
  .hint{font-size:11px;color:var(--muted);margin-top:3px}
  .row{display:flex;gap:8px;align-items:center}
  .btn{padding:9px 16px;border-radius:8px;border:0;cursor:pointer;font-size:13px;font-weight:600}
  .btn-primary{background:var(--blue);color:#fff}
  .btn-primary:disabled{opacity:.5;cursor:not-allowed}
  .btn-ghost{background:transparent;color:var(--muted);border:1px solid var(--line)}
  .btn-ghost:hover{color:var(--text)}
  .status{margin-top:10px;font-size:12px;min-height:18px}
  .status.ok{color:var(--green)} .status.err{color:var(--red)} .status.info{color:var(--muted)}
  .token-toggle{margin:6px 0}
  .token-toggle a{color:var(--blue);cursor:pointer;font-size:12px;text-decoration:none}
  .secret-mode{display:none}
  .step-done{color:var(--green)}
  .cfg-table{width:100%;border-collapse:collapse;font-size:12px}
  .cfg-table td{padding:5px 8px;border-bottom:1px solid var(--line);vertical-align:top}
  .cfg-key{color:var(--muted);width:90px;white-space:nowrap}
  .cfg-val{color:var(--text);word-break:break-all}
  .cfg-val.empty{color:var(--muted)}
  /* ① 选岗位：勾选预装 + 单选默认岗位 */
  .job-list{display:flex;flex-wrap:wrap;gap:6px;margin-top:8px}
  .job-chip{display:inline-flex;align-items:center;gap:5px;padding:5px 10px;border:1px solid var(--line);border-radius:16px;background:#161d2e;color:var(--text);font-size:12px;cursor:pointer;user-select:none}
  .job-chip.checked{border-color:var(--green);color:var(--green);background:#1c2a24}
  .job-chip .tick{width:14px;text-align:center}
  .job-chip input{display:none}
  .job-radio{display:inline-flex;align-items:center;gap:5px;margin-right:10px;font-size:12px;color:var(--text);cursor:pointer}
  .job-radio input{margin:0}
  .job-radio.radio{width:auto}
  .job-hint{font-size:11px;color:var(--muted);margin:6px 0 8px}
</style>
</head>
<body>
  <h1>🤖 配置数字分身</h1>
  <div class="sub">推荐：点下面「自动激活」，用公司账号一键认领你的数字分身（无需手填任何信息）。</div>

  <div class="section" id="activateSection" style="border-color:var(--green)">
    <h2 style="color:var(--green)">⭐ 自动激活（推荐）</h2>
    <div class="hint" style="margin-bottom:10px">用公司 SSO（Keycloak）登录，自动认领你的 @ai-你的邮箱前缀 数字分身。无需密码、无需 token。</div>
    <button class="btn btn-primary" id="activateBtn" style="width:100%;background:var(--green)">🚀 自动激活数字分身</button>
    <div class="status" id="activateStatus"></div>
  </div>

  <div class="section">
    <h2 style="color:var(--blue)">📡 当前连接配置（服务端下发，只读）</h2>
    <div class="hint" style="margin-bottom:10px">以下连接参数由服务端统一下发（激活时自动写入），本地不可手改，用于排查连接问题。</div>
    <table class="cfg-table">
      <tr><td class="cfg-key">服务器地址</td><td class="cfg-val" id="viewHs">—</td></tr>
      <tr><td class="cfg-key">分身账号</td><td class="cfg-val" id="viewUid">—</td></tr>
      <tr><td class="cfg-key">主人账号</td><td class="cfg-val" id="viewOwner">—</td></tr>
      <tr><td class="cfg-key">访问令牌</td><td class="cfg-val" id="viewToken">—</td></tr>
    </table>
  </div>

  <div class="section" id="manualSection" style="display:none">
    <h2 style="color:var(--amber)">🛠 手动配置（开发者）</h2>
    <div class="hint" style="margin-bottom:10px">已启用开发者手动配置开关（matrixManualConfig）。此处可手填连接参数，覆盖服务端下发值，用于测试环境联调。</div>
    <label>服务器地址（homeserverUrl）</label>
    <input id="mHs" placeholder="https://matrix.example.com">
    <label>分身账号（userId）</label>
    <input id="mUid" placeholder="@ai-xxx:example.com">
    <label>主人账号（owner，可选）</label>
    <input id="mOwner" placeholder="@owner:example.com">
    <label>访问令牌（accessToken）</label>
    <input id="mToken" type="password" placeholder="syt_...">
    <div class="token-toggle"><a id="toggleToken" onclick="return false">显示令牌</a></div>
    <button class="btn btn-primary" id="submitBtn" style="width:100%">保存手动配置</button>
    <div class="status" id="manualStatus"></div>
  </div>

  <div class="section" id="pendingSection" style="display:none">
    <h2 style="color:var(--amber)">📋 订阅 / 安装清单</h2>
    <div class="hint" style="margin-bottom:8px">你的账号已订阅以下能力，但本地尚未安装或版本落后。点「安装 / 修复」会自动补齐。</div>
    <div id="pendingList" style="font-size:12px"></div>
  </div>

  <div class="section" id="jobSection" style="display:none">
    <h2 style="color:var(--blue)">💼 选择岗位（重点）</h2>
    <div class="hint">勾选要**预装**的岗位（会下载对应岗位技能，默认全选，可去掉不需要的）；再选一个**默认岗位**（分身激活后默认以它开工）。</div>
    <div class="job-hint">预装岗位：</div>
    <div class="job-list" id="jobList"></div>
    <div class="job-hint" style="margin-top:12px">默认岗位（上岗后默认启用哪一个）：</div>
    <div id="defaultJobRadios" style="font-size:12px"></div>
    <button class="btn btn-primary" id="saveJobsBtn" style="width:100%;margin-top:14px">💾 保存岗位设置并完成</button>
    <div class="status" id="jobStatus"></div>
  </div>

  <div class="status" id="status"></div>

<script>
(function(){
  const $=id=>document.getElementById(id);
  const esc=s=>String(s).replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;");

  // 只读展示：加载当前连接配置（服务端下发 + 激活写入），用于排查，不允许本地手改
  fetch("http://matrix-setup.localhost/state").then(r=>r.json()).then(s=>{
    $("viewHs").textContent = s.homeserver_url || "（未下发）";
    $("viewUid").textContent = s.user_id || "（未激活）";
    $("viewOwner").textContent = s.owner || "（未设置）";
    $("viewToken").textContent = s.access_token_set ? "已写入（明文不在此展示）" : "（未写入）";
    if(s.status==="configured"){ $("status").innerHTML='<span class="ok">✓ 已配置并连接。</span>'; }
    else if(s.status==="not-installed"){ $("status").innerHTML='<span class="err">数字分身插件未安装——请先关闭本窗口，在托盘点「安装 / 修复」。</span>'; }
    else if(s.status==="unconfigured"){ $("status").innerHTML='<span class="info">尚未完成激活。请点上方「自动激活」。</span>'; }
    // 开发者手动配置开关：matrixManualConfig=true 时显示手填区块并预填当前值
    if(s.manual_config_enabled){
      $("manualSection").style.display="block";
      $("mHs").value = s.homeserver_url || "";
      $("mUid").value = s.user_id || "";
      $("mOwner").value = s.owner || "";
      // access token 不回传明文，仅占位提示
      $("mToken").placeholder = s.access_token_set ? "已设置（留空则保持不变）" : "syt_...";
    }
    // 订阅/安装清单差异
    if(s.pending_plugins && s.pending_plugins.length){
      const list=$("pendingList"); list.innerHTML="";
      s.pending_plugins.forEach(p=>{
        const tag = p.action==="update"
          ? '<span style="color:var(--amber)">待更新</span>'
          : '<span style="color:var(--blue)">待安装</span>';
        const ver = p.installed ? ` <span style="color:var(--muted)">（已装 ${esc(p.installed)} → ${esc(p.latest||"最新")}）</span>` : "";
        const div=document.createElement("div");
        div.style.cssText="padding:4px 0;border-bottom:1px solid var(--line)";
        div.innerHTML=`${tag} <strong>${esc(p.name)}</strong>${ver}`;
        list.appendChild(div);
      });
      $("pendingSection").style.display="block";
    }
    // 岗位选择：仅在「已激活」（status=configured）且有服务端下发的候选岗位时展示。
    // 未激活时（unconfigured/not-installed）不展示——先完成激活再选岗位。
    if(s.status==="configured"){ renderJobSection(s); }
  }).catch(()=>{});

  // 渲染岗位选择区块。candidates = 服务端 jobPresets；defaultJob = 当前已落盘默认岗位。
  // 首次激活后：默认全选 + 默认岗位为空（等用户选）；已保存过：回显当前值。
  let jobPresets = [];      // 服务端候选（默认全选基础）
  let jobSelected = new Set();
  let jobDefault = "";
  let jobInitialized = false;
  function renderJobSection(s){
    const cands = s.job_presets || [];
    if(!cands.length){ $("jobSection").style.display="none"; return; }
    if(jobInitialized){ return; }  // 只初始化一次，避免轮询/重复渲染覆盖用户已选
    jobPresets = cands;
    jobSelected = new Set(cands);          // 默认全选
    jobDefault = s.default_job || "";      // 回显当前默认岗位（可为空）
    jobInitialized = true;
    paintJobChips();
    $("jobSection").style.display="block";
  }
  function paintJobChips(){
    // 预装岗位勾选 chips
    const list=$("jobList"); list.innerHTML="";
    jobPresets.forEach(j=>{
      const on=jobSelected.has(j);
      const chip=document.createElement("div");
      chip.className="job-chip"+(on?" checked":"");
      chip.innerHTML='<span class="tick">'+(on?"✓":"○")+'</span>'+esc(j);
      chip.onclick=()=>{
        if(jobSelected.has(j)) jobSelected.delete(j); else jobSelected.add(j);
        paintJobChips();
      };
      list.appendChild(chip);
    });
    // 默认岗位单选
    const radios=$("defaultJobRadios"); radios.innerHTML="";
    const mkRadio=(val,label,checked)=>{
      const lab=document.createElement("label");
      lab.className="job-radio";
      lab.innerHTML='<input class="radio" type="radio" name="defaultJob" value="'+esc(val)+'"'+(checked?" checked":"")+'>'+esc(label);
      const inp=lab.querySelector("input");
      inp.onchange=()=>{ jobDefault=val; };
      lab.style.marginRight="12px";
      radios.appendChild(lab);
    };
    mkRadio("","不指定", jobDefault==="");
    jobPresets.forEach(j=>{ mkRadio(j, j, jobDefault===j); });
  }

  // 保存岗位设置：勾选的预装清单 + 默认岗位 → POST /jobs → 关窗
  $("saveJobsBtn").onclick=async()=>{
    const st=$("jobStatus"); st.className="status info"; st.textContent="正在保存岗位设置…";
    $("saveJobsBtn").disabled=true;
    const payload={ jobs:Array.from(jobSelected), defaultJob:jobDefault };
    try{
      const r=await fetch("http://matrix-setup.localhost/jobs",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify(payload)});
      const j=await r.json();
      if(j.ok){
        st.innerHTML='<span class="ok">✓ 岗位设置已保存，向导即将关闭。</span>';
      } else { st.className="status err"; st.textContent="✗ "+(j.error||"保存失败"); $("saveJobsBtn").disabled=false; }
    }catch(e){ st.className="status err"; st.textContent="✗ 无法连接本机服务"; $("saveJobsBtn").disabled=false; }
  };

  // 自动激活：调本机服务，打开浏览器授权。激活成功后，若有岗位候选则轮询 /state
  // 等 status=configured 后展示「选岗位」步骤；无岗位候选则由后端关窗。
  $("activateBtn").onclick=async()=>{
    const st=$("activateStatus"); st.className="status info"; st.textContent="正在打开浏览器授权…";
    $("activateBtn").disabled=true;
    try{
      const r=await fetch("http://matrix-setup.localhost/activate",{method:"POST",headers:{"Content-Type":"application/json"},body:"{}"});
      const j=await r.json();
      if(j.ok){
        st.innerHTML='<span class="ok">✓ 已启动！浏览器即将打开，请完成公司 SSO 登录。完成后本窗口会引导你选择岗位。</span>';
        // 轮询 /state，等激活完成（configured）后渲染岗位区块；
        // 检测 last_activation_error 感知失败（恢复按钮 + 提示，避免「失败后按钮锁死」）
        let tries=0;
        const poll=setInterval(async()=>{
          tries++;
          if(tries>90){ clearInterval(poll); $("activateBtn").disabled=false; st.className="status err"; st.textContent="✗ 激活超时未完成，请重试。"; return; }  // 最多 135s
          try{
            const s=await (await fetch("http://matrix-setup.localhost/state")).json();
            if(s.last_activation_error){
              clearInterval(poll);
              st.className="status err";
              st.textContent="✗ "+(s.last_activation_error||"激活失败，请重试");
              $("activateBtn").disabled=false;
              return;
            }
            if(s.status==="configured"){
              clearInterval(poll);
              st.className="status ok";
              st.textContent="✓ 已激活！";
              if(s.job_presets && s.job_presets.length){
                renderJobSection(s);
              }
            }
          }catch(e){}
        },1500);
      } else { st.className="status err"; st.textContent="✗ "+(j.error||"激活启动失败"); $("activateBtn").disabled=false; }
    }catch(e){ st.className="status err"; st.textContent="✗ 无法连接本机服务"; $("activateBtn").disabled=false; }
  };

  // 开发者手动配置：显示令牌切换 + 提交
  $("toggleToken").onclick=()=>{ const el=$("mToken"); el.type = el.type==="password" ? "text" : "password"; };
  $("submitBtn").onclick=async()=>{
    const st=$("manualStatus"); st.className="status info"; st.textContent="正在写入配置…";
    $("submitBtn").disabled=true;
    const payload={
      homeserverUrl:$("mHs").value.trim(),
      userId:$("mUid").value.trim(),
      owner:$("mOwner").value.trim(),
      accessToken:$("mToken").value.trim()
    };
    try{
      const r=await fetch("http://matrix-setup.localhost/submit",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify(payload)});
      const j=await r.json();
      if(j.ok){
        st.innerHTML='<span class="ok">✓ 已提交！本窗口会自动关闭，进度在操作窗口显示。</span>';
      } else { st.className="status err"; st.textContent="✗ "+(j.error||"提交失败"); $("submitBtn").disabled=false; }
    }catch(e){ st.className="status err"; st.textContent="✗ 无法连接本机服务"; $("submitBtn").disabled=false; }
  };
})();
</script>
</body>
</html>"#.to_string();
    html
}

// ---------- 测试 ----------
#[cfg(test)]
mod tests {
    use super::*;

    fn sample_settings_with_mirror() -> String {
        // 模拟 dsh 已写入大量运行时镜像的 settings.yaml
        "dsh-matrix:\n  homeserverUrl: 'https://old.example'\n  timelineSnapshot:\n    entries: []\n    updatedAt: 123\n  tasksSnapshot:\n    rooms: {}\n".to_string()
    }

    #[test]
    fn write_preserves_mirror_keys_and_updates_account() {
        let dir = std::env::temp_dir().join(format!("dsh-setup-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.yaml");
        std::fs::write(&path, sample_settings_with_mirror()).unwrap();
        let acc = MatrixAccount {
            homeserver_url: "https://im.example".into(),
            user_id: "@ai-x:example".into(),
            access_token: "tok-123".into(),
            owner: "@owner:example".into(),
        };
        write_account_to_file(&path, &acc).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        // 账号键已更新
        assert!(text.contains("homeserverUrl: https://im.example"));
        assert!(text.contains("userId: '@ai-x:example'"));
        assert!(text.contains("accessToken: tok-123"));
        // 镜像键保留
        assert!(text.contains("timelineSnapshot"));
        assert!(text.contains("tasksSnapshot"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_creates_file_when_absent() {
        let dir = std::env::temp_dir().join(format!("dsh-setup-create-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.yaml");
        let acc = MatrixAccount {
            homeserver_url: "https://im.example".into(),
            user_id: "@ai-x:example".into(),
            access_token: "t".into(),
            owner: String::new(),
        };
        write_account_to_file(&path, &acc).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("dsh-matrix:"));
        assert!(text.contains("homeserverUrl"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_empties_account_keeps_mirror() {
        let dir = std::env::temp_dir().join(format!("dsh-setup-clear-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.yaml");
        std::fs::write(&path, sample_settings_with_mirror()).unwrap();
        clear_account_in_file(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("timelineSnapshot"));
        // 账号键被置空（homeserverUrl: ''）
        assert!(text.contains("homeserverUrl: ''") || text.contains("homeserverUrl: \"\""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_ns_reads_account_fields() {
        let yaml = "dsh-matrix:\n  homeserverUrl: 'https://a'\n  userId: '@u:a'\n  accessToken: 't1'\n  owner: '@o:a'\n  timelineSnapshot: {}\n";
        let acc = parse_ns(yaml, MATRIX_NS).unwrap();
        assert_eq!(acc.homeserver_url, "https://a");
        assert_eq!(acc.user_id, "@u:a");
        assert_eq!(acc.access_token, "t1");
        assert_eq!(acc.owner, "@o:a");
    }

    // ---------- HiMarket SSO 登录态读写 ----------

    #[test]
    fn himarket_write_preserves_other_keys() {
        let dir = std::env::temp_dir().join(format!("dsh-hm-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.yaml");
        // 模拟已有 himarket 配置（含非登录键，必须保留）
        std::fs::write(
            &path,
            "himarket:\n  baseUrl: 'http://market.example'\n  gatewayUrl: 'http://job.example'\n  portalId: 'p-1'\n  skillInstallDir: '/x'\n",
        )
        .unwrap();
        let login = HimarketLogin {
            base_url: "http://market.ai.ict.cmcc".into(),
            username: "niukunliang".into(),
            display_name: "牛昆亮".into(),
            token: "tok-abc".into(),
        };
        write_himarket_login_to_file(&path, &login).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("token: tok-abc"));
        assert!(text.contains("username: niukunliang"));
        assert!(text.contains("displayName: 牛昆亮"));
        assert!(text.contains("baseUrl: http://market.ai.ict.cmcc"));
        // 非登录键必须原样保留
        assert!(text.contains("gatewayUrl: http://job.example"));
        assert!(text.contains("portalId: p-1"));
        assert!(text.contains("skillInstallDir: /x"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn himarket_write_skips_empty_values() {
        // SSO 登录缺 username 时，不应把用户已填的兜底账号清掉
        let dir = std::env::temp_dir().join(format!("dsh-hm-empty-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.yaml");
        std::fs::write(&path, "himarket:\n  username: 'user'\n  password: 'pw'\n").unwrap();
        let login = HimarketLogin {
            base_url: String::new(),
            username: String::new(),
            display_name: String::new(),
            token: "tok-new".into(),
        };
        write_himarket_login_to_file(&path, &login).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("token: tok-new"));
        assert!(text.contains("username: user"), "空 username 不应覆盖已填值");
        assert!(text.contains("password: pw"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn himarket_token_state_detects_login() {
        let dir = std::env::temp_dir().join(format!("dsh-hm-state-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.yaml");

        // 无文件 → 未登录
        assert_eq!(himarket_token_state_in_file(&path), HimarketTokenState::NotLoggedIn);

        // token 为空 → 未登录
        std::fs::write(&path, "himarket:\n  token: ''\n  username: 'u'\n").unwrap();
        assert_eq!(himarket_token_state_in_file(&path), HimarketTokenState::NotLoggedIn);

        // token 非空 → 已登录（带用户名）
        std::fs::write(&path, "himarket:\n  token: 't'\n  username: 'niukunliang'\n").unwrap();
        assert_eq!(
            himarket_token_state_in_file(&path),
            HimarketTokenState::LoggedIn {
                username: "niukunliang".into(),
                display_name: String::new(),
            }
        );

        // 带中文姓名（SSO 登录后）→ 一并读出
        std::fs::write(&path, "himarket:\n  token: 't'\n  username: 'niukunliang'\n  displayName: 牛昆亮\n").unwrap();
        assert_eq!(
            himarket_token_state_in_file(&path),
            HimarketTokenState::LoggedIn {
                username: "niukunliang".into(),
                display_name: "牛昆亮".into(),
            }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------- 员工身份（管理页「谁在用这台机器」）----------

    #[test]
    fn identity_reads_sso_username_and_display_name() {
        let dir = std::env::temp_dir().join(format!("dsh-id-sso-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.yaml");
        std::fs::write(
            &path,
            "himarket:\n  username: niukunliang\n  displayName: 牛昆亮\n  token: t\n\
             dsh-matrix:\n  userId: '@ai-niukunliang:im.ai.ict.cmcc'\n  owner: '@niukunliang:im.ai.ict.cmcc'\n",
        )
        .unwrap();
        let id = read_identity_from_file(&path);
        assert_eq!(id.username, "niukunliang");
        assert_eq!(id.display_name, "牛昆亮");
        assert_eq!(id.owner, "@niukunliang:im.ai.ict.cmcc");
        assert_eq!(id.twin_user_id, "@ai-niukunliang:im.ai.ict.cmcc");
        assert!(id.any());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn identity_falls_back_to_owner_when_not_sso_logged_in() {
        // 未 SSO 登录（himarket 无 username），但分身已配置 → 从 owner 反推账号名
        let dir = std::env::temp_dir().join(format!("dsh-id-owner-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.yaml");
        std::fs::write(
            &path,
            "dsh-matrix:\n  userId: '@ai-niukunliang:im.ai.ict.cmcc'\n  owner: '@niukunliang:im.ai.ict.cmcc'\n",
        )
        .unwrap();
        let id = read_identity_from_file(&path);
        assert_eq!(id.username, "niukunliang", "owner 反推账号名");
        assert!(id.display_name.is_empty(), "无 SSO 时没有中文姓名");
        assert!(id.any());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn identity_empty_when_nothing_configured() {
        let dir = std::env::temp_dir().join(format!("dsh-id-none-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.yaml");

        // 文件不存在 → 空身份，且 any()=false（管理页显示「未登录」）
        let id = read_identity_from_file(&path);
        assert!(!id.any(), "无文件时不应报告身份");

        // 文件存在但无关键 → 同样空身份
        std::fs::write(&path, "llm:\n  retries: 3\n").unwrap();
        let id = read_identity_from_file(&path);
        assert!(!id.any());
        assert_eq!(id, ClientIdentity::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn identity_ignores_twin_account_prefix_in_owner_fallback() {
        // 反推只在 owner 上做：分身 userId 是机器账号（@ai-xxx），不能当人名用
        assert_eq!(localpart_of("@niukunliang:im.ai.ict.cmcc"), "niukunliang");
        assert_eq!(localpart_of("@ai-niukunliang:im.ai.ict.cmcc"), "ai-niukunliang");
        // 非 Matrix userId 形状 → 空串（不猜）
        assert_eq!(localpart_of(""), "");
        assert_eq!(localpart_of("niukunliang"), "");
        assert_eq!(localpart_of("@nocolon"), "");
        assert_eq!(localpart_of("@:im.ai.ict.cmcc"), "");
    }

    #[test]
    fn identity_json_uses_camel_case() {
        // 上报给中心服务端的字段名是 camelCase（与 sync.rs 的 json! 一致）
        let id = ClientIdentity {
            username: "niukunliang".into(),
            display_name: "牛昆亮".into(),
            owner: "@niukunliang:im.ai.ict.cmcc".into(),
            twin_user_id: "@ai-niukunliang:im.ai.ict.cmcc".into(),
        };
        let json = serde_json::to_string(&id).unwrap();
        assert!(json.contains("\"displayName\":\"牛昆亮\""), "got {json}");
        assert!(json.contains("\"twinUserId\""), "got {json}");
        assert!(json.contains("\"username\":\"niukunliang\""), "got {json}");
    }

    #[test]
    fn himarket_clear_keeps_baseurl() {
        let dir = std::env::temp_dir().join(format!("dsh-hm-clear-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.yaml");
        std::fs::write(
            &path,
            "himarket:\n  baseUrl: 'http://m'\n  token: 't'\n  username: 'u'\n  portalId: 'p'\n",
        )
        .unwrap();
        clear_himarket_login_in_file(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("token: ''") || text.contains("token: \"\""));
        assert!(text.contains("baseUrl: http://m"), "baseUrl 应保留");
        assert!(text.contains("portalId: p"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn account_complete_and_missing() {
        let full = MatrixAccount {
            homeserver_url: "https://a".into(),
            user_id: "@u:a".into(),
            access_token: "t".into(),
            owner: String::new(),
        };
        assert!(full.complete());
        assert!(full.missing().is_empty());

        let no_token = MatrixAccount {
            homeserver_url: "https://a".into(),
            user_id: "@u:a".into(),
            access_token: String::new(),
            owner: String::new(),
        };
        assert!(!no_token.complete());
        assert_eq!(no_token.missing(), vec!["accessToken"]);

        // 占位 token 视为未配置
        let pending = MatrixAccount {
            homeserver_url: "https://a".into(),
            user_id: "@u:a".into(),
            access_token: PENDING_CONFIG.into(),
            owner: String::new(),
        };
        assert!(!pending.complete());
    }

    #[test]
    fn parse_preset_patch_extracts_config() {
        let patch = "# bundle layer\n- insert:\n    - id: matrix\n      name: dsh-matrix-agent\n      config:\n        homeserverUrl: 'https://im.example'\n        userId: '@ai-x:example'\n        accessToken: ''\n        owner: '@owner:example'\n";
        let acc = parse_preset_patch(patch);
        assert_eq!(acc.homeserver_url, "https://im.example");
        assert_eq!(acc.user_id, "@ai-x:example");
        assert_eq!(acc.owner, "@owner:example");
        assert_eq!(acc.access_token, "");
    }
}
