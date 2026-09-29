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
    /// 已装好运行环境，但还未激活数字分身（等待领号）。
    ReadyToActivate,
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

/// 当前向导状态（两阶段模型）。
///
/// 阶段一「安装运行环境」：dsh 核心（node/pnpm/dsh 二进制）+ matrix profile 骨架
/// （web-app + dsh-matrix-agent 桥）都装好。
/// 阶段二「领号启动」：账号配置（homeserverUrl/userId/accessToken）写入完成。
///
/// - [`MatrixStatus::NotInstalled`]：阶段一未完成（环境缺东西，需先安装）；
/// - [`MatrixStatus::Unconfigured`]：阶段一完成、阶段二未做（已就绪，等待领号）；
/// - [`MatrixStatus::Configured`]：两阶段全部完成，分身可用。
///
/// 关于「已激活未重启」中间态：账号三要素齐全（settings.yaml 已写）但 matrix
/// profile 进程尚未运行，属于「激活成功、等待用户确认重启」的中间态。该态在
/// `status()` 里仍归入 [`MatrixStatus::Configured`]（配置层面已完整），但在
/// [`collect_state`] 里通过 `activated_not_launched` 字段单独暴露给前端，避免
/// 前端把「已激活未重启」误判成「还没激活」或「已全部完成」。
pub fn status<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> MatrixStatus {
    // ① 阶段一：运行环境未装（dsh 二进制不在）→ 需先安装
    if !dsh_binary_path(app).exists() {
        return MatrixStatus::NotInstalled;
    }
    // ② 阶段一：运行环境在，但 matrix profile 骨架/桥未就绪 → 也需安装补全
    if !matrix_agent_installed(app, cfg) {
        return MatrixStatus::NotInstalled;
    }
    // ③ 阶段二：环境就绪，检查账号
    let (acc, _) = load_account(app, cfg);
    if acc.complete() {
        MatrixStatus::Configured
    } else if manual_config_enabled(cfg) {
        // 手动配置模式：给全缺字段细节（开发者联调用）
        MatrixStatus::Unconfigured {
            missing: acc.missing().iter().map(|s| s.to_string()).collect(),
        }
    } else {
        // 自动化模式：环境已就绪，等待领号（阶段二）
        MatrixStatus::ReadyToActivate
    }
}

/// 「已激活但未重启」中间态判定：账号三要素齐全（settings.yaml 已写、分身已认领），
/// 但 matrix profile 进程尚未运行。用于让向导前端感知「激活完成、该展示账号并让
/// 用户确认重启」——区别于「还没激活」（三要素不齐）和「已全部完成」（进程在跑）。
pub fn activated_not_launched<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> bool {
    let (acc, _) = load_account(app, cfg);
    if !acc.complete() {
        return false;
    }
    let running = crate::workflow::is_running()
        && crate::workflow::current_profile().as_deref() == Some(MATRIX_PROFILE);
    !running
}

/// dsh-matrix-agent 是否已装进 matrix profile（node_modules 存在 bundle patch）。
///
/// 判定口径（2026-09-24 起，两阶段模型的「安装成功」确定性判据）：
/// 同时认两个包布局（pnpm 结构兼容）：
/// - `profiles/matrix/node_modules/dsh-matrix-agent/cordis.patch.yml`（顶层包）
/// - `profiles/matrix/node_modules/@deepseek-ai/dsh-matrix-agent/cordis.patch.yml`（scoped）
/// 任一命中即视为「已安装」。文件存在是**客观安装事实**，比「日志字符串」可靠得多。
pub fn matrix_agent_installed<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> bool {
    let nm = dsh_home(app, cfg).join("profiles").join(MATRIX_PROFILE).join("node_modules");
    nm.join(MATRIX_AGENT_BUNDLE).join("cordis.patch.yml").exists()
        || nm.join("@deepseek-ai").join(MATRIX_AGENT_BUNDLE).join("cordis.patch.yml").exists()
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

/// 等待 Matrix 桥连接就绪（两阶段模型的「运行成功」确定性判据）。
///
/// 判据（按优先级）：
/// 1. **文件事实**：`<DSH_HOME>/.dsh-matrix/diagnostics.log` 出现成功信号
///    `Matrix bridge started`（bridge.js 启动成功后才写这行——这是「桥真正连上
///    Matrix」的客观信号，比「端口活着」强）。
/// 2. **快速失败**：dsh-matrix-agent 进程已死（端口/进程探测失败）→ 立即报错，
///    不傻等满 45s。
/// 3. 附带提示：diagnostics.log 文件不存在 = 桥根本没加载（matrix profile 里
///    dsh-matrix-agent 未装 / patch 缺失）→ 给明确诊断，而不是「等超时」。
///
/// 超时返回错误（可重试）。返回的 Err 文案就是直接给用户看的原因。
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
    let mut file_ever_seen = false;
    while std::time::Instant::now() < deadline {
        // ① 文件事实判据：出现成功信号即完成
        if let Ok(text) = std::fs::read_to_string(&diag_path) {
            file_ever_seen = true;
            // 全文判断（不做字节切片，避免切中文 panic）
            let full = text.as_str();
            // 出现成功信号：bridge started / Matrix bridge started
            let started = full.contains("bridge started") || full.contains("Matrix bridge started");
            let incomplete = full.contains("incomplete config") || full.contains("not started");
            let configured = full.contains("config complete") || full.contains("starting Matrix bridge");
            if started || (configured && !incomplete) {
                log::info!("[matrix-setup] Matrix 桥已连接（诊断日志确认）");
                return Ok(());
            }
        }
        // ② 快速失败：dsh-matrix-agent 进程已死（端口/进程探测失败）→ 立即报错
        if !crate::workflow::is_running() {
            return Err("数字分身进程未在运行，无法建立 Matrix 连接。请稍后在托盘「启动」重试。".to_string());
        }
        std::thread::sleep(std::time::Duration::from_millis(1500));
    }
    // ③ 超时：区分「文件从未出现」（桥未加载）与「文件有但没到成功信号」（连接慢）
    if !file_ever_seen {
        Err(format!(
            "等待 Matrix 连接超时：未找到诊断日志（{}）。\
             可能原因：dsh-matrix-agent 桥未加载。请在托盘「安装 / 修复」补装插件后重试。",
            diag_path.display()
        ))
    } else {
        Err("等待 Matrix 连接超时——请查看日志确认数字分身配置是否正确".to_string())
    }
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
        let html = wizard_html(app);
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
        // 单窗口一贯流程：任务移交后台，进度写入 ops 全局状态，由向导窗口内嵌
        // 进度区轮询 /op-state 展示——不再弹独立「操作进度」窗、也不关闭向导。
        let h = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            // ① dsh 未装 → 先装依赖（进度内嵌于向导，无需独立进度窗；install_all
            //    已做「向导存在则不弹 console」）。装完才能激活。
            if !dsh_binary_path(&h).exists() {
                crate::ops::append_log(&h, "[开始] 检测到运行环境未安装，先安装依赖…");
                let _ = tauri::async_runtime::block_on(crate::install::install_all(&h));
            }
            crate::ops::start_op(&h, "matrix-activate", "自动激活数字分身", &["浏览器授权", "认领身份", "写入配置", "等待确认"]);
            crate::ops::mark_step_running(&h, 0);
            crate::ops::update_step(&h, "等待浏览器授权…");
            crate::ops::append_log(&h, "已打开浏览器，请在浏览器中完成公司 SSO 登录…");
            match crate::activation::run_activation(&h) {
                r if r.ok => {
                    crate::ops::mark_step_running(&h, 2);
                    crate::ops::update_step(&h, "已认领分身，写入配置…");
                    crate::ops::append_log(&h, &format!("✓ 已认领分身账号 {}", r.user_id));
                    // ⭐ 拆分重启：run_activation 只做「授权 → 认领 → 写 settings.yaml」，
                    // 这里**不再自动重启数字分身**。仅确保 profile 骨架存在（为后续用户
                    // 确认后调 /launch 启动铺路），然后把「激活成功但未重启」的状态交给
                    // 前端——前端展示分身账号、等用户核对确认，确认后才调 /launch 真正重启。
                    let cfg_now = load_cached();
                    if let Err(e) = crate::install::ensure_matrix_profile(&h, &cfg_now) {
                        crate::ops::append_log(&h, &format!("⚠️ 确保数字分身运行环境失败：{e}"));
                    }
                    // 激活成功但尚未重启：清空「最近激活失败」标记（前端轮询感知到
                    // activated_not_launched=true 即收尾到「确认重启」区）。
                    *LAST_ACTIVATION_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    crate::ops::mark_step_running(&h, 3);
                    crate::ops::update_step(&h, "已写入配置，等待确认重启…");
                    crate::ops::finish_op(&h, &format!("分身已激活（{}），已写入配置，等待你确认后重启", r.user_id));
                    crate::notify::notify(&h, "数字分身已激活", &format!("{} 已写入配置。请在向导中核对账号并确认重启。", r.user_id));
                    crate::tray::refresh_sync_menu(&h);
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
        // ===== 优化 A：进度与岗位选择全部收进「同一个向导窗口」 =====
        // 不再开第二个「操作进度」窗交替闪烁：
        // - 有岗位候选 → 向导窗口直接切换到「选岗位」步骤（jobSection 由前端渲染）；
        // - 无岗位候选 → 进度在向导窗口内嵌区显示（前端轮询 /op-state），完成后由后端收尾。
        // 仅当向导窗口意外未打开时，重新打开/聚焦向导（进度依旧内嵌，**不弹独立进度窗**）。
        let h = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let _ = crate::matrix_setup::open_window(&h); // 聚焦/重建向导（进度内嵌）
        });
        return json_resp(StatusCode::OK, serde_json::json!({"ok": true, "message": "已开始自动激活"}));
    }
    if method == tauri::http::Method::POST && path == "/resume-auto-activation" {
        // ⚠️ 已废弃（2026-09）：first_run 安装完成后的「自动衔接」已去掉，本端点已无调用方。
        // 保留仅为向后兼容 + 历史语义记录，勿在此继续维护重启逻辑。当前激活主流程走
        // /activate（激活后停在「确认重启」，由前端确认后调 /launch 才真正重启）。
        // first_run 安装完成自动流转入口：不弹通知，直接衔接「激活」流程
        // 复用与 /activate 相同的后台线程逻辑，仅不对前端再弹「已开始」
        let h = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            // 进度归到 ops（进度 HUD 与向导共用同一套 ops 接口）
            crate::ops::start_op(&h, "auto-resume-activate", "安装后自动激活并启动（无人值守）", &["浏览器授权（无人值守）", "认领身份", "写入配置", "重启数字分身", "等待连接"]);
            crate::ops::mark_step_running(&h, 0);
            crate::ops::update_step(&h, "等待浏览器授权…");
            crate::ops::append_log(&h, "已打开浏览器，请在浏览器中完成公司 SSO 登录…");
            match crate::activation::run_activation(&h) {
                r if r.ok => {
                    crate::ops::mark_step_running(&h, 2);
                    crate::ops::update_step(&h, "已认领分身，写入配置…");
                    crate::ops::append_log(&h, &format!("✓ 已认领分身账号 {}", r.user_id));
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
                            *LAST_ACTIVATION_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = None;
                            crate::ops::append_log(&h, &format!("✓ 数字分身已启动：{}", crate::workflow::access_url(port)));
                            crate::ops::mark_step_running(&h, 4);
                            crate::ops::update_step(&h, "等待 Matrix 连接…");
                            match wait_matrix_ready(&h, &cfg_now, std::time::Duration::from_secs(45)) {
                                Ok(()) => {
                                    crate::ops::finish_op(&h, &format!("数字分身已激活并自动就绪：{}", r.user_id));
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
                            *LAST_ACTIVATION_ERROR.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(format!("分身已激活但启动失败：{e}"));
                        }
                    }
                }
                r => {
                    crate::ops::fail_op(&h, &r.message);
                    crate::notify::notify(&h, "自动激活失败", &r.message);
                    *LAST_ACTIVATION_ERROR.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(r.message.clone());
                }
            }
        });
        // 向导层面由前端控制自动关闭/提示，这里不额外动窗
        return json_resp(StatusCode::OK, serde_json::json!({"ok": true, "message": "已开始自动激活（安装后无人值守）"}));
    }
    if method == tauri::http::Method::POST && path == "/launch" {
        // 「数字分身已就绪」区块的启动入口：configured 态（账号已激活）直接启动
        // matrix 分身。**不触发 SSO**（区别于 /activate——那是「领号」流程，会
        // 弹浏览器重领；已激活机器不该再领号）。启动逻辑与托盘「启动 Harness」
        // 一致：确保 profile 骨架 → 幂等启动（已运行同 profile 直接返回）→ 等连接。
        let h = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            crate::ops::start_op(&h, "launch", "启动数字分身", &["确保运行环境", "启动分身", "等待连接"]);
            crate::ops::mark_step_running(&h, 0);
            crate::ops::update_step(&h, "确保运行环境…");
            let cfg_now = load_cached();
            if let Err(e) = crate::install::ensure_matrix_profile(&h, &cfg_now) {
                crate::ops::append_log(&h, &format!("⚠️ 确保数字分身运行环境失败：{e}"));
            }
            crate::ops::mark_step_running(&h, 1);
            crate::ops::update_step(&h, "启动数字分身…");
            match crate::workflow::launch_with_profile(&h, MATRIX_PROFILE) {
                Ok(port) => {
                    crate::ops::append_log(&h, &format!("✓ 数字分身已启动：{}", crate::workflow::access_url(port)));
                    crate::ops::mark_step_running(&h, 2);
                    crate::ops::update_step(&h, "等待 Matrix 连接…");
                    match wait_matrix_ready(&h, &cfg_now, std::time::Duration::from_secs(45)) {
                        Ok(()) => {
                            crate::ops::finish_op(&h, "数字分身已启动并可用（Matrix 已连接）");
                            crate::tray::refresh_sync_menu(&h);
                            crate::notify::notify(&h, "数字分身已启动", "数字分身已在托盘运行，可在 Matrix 客户端 @ 它试试。");
                        }
                        Err(e) => {
                            crate::ops::finish_op(&h, &format!("数字分身已启动，但连接等待超时：{e}"));
                            crate::notify::notify(&h, "数字分身已启动", &format!("进程已启动。连接验证超时（不影响使用）：{e}"));
                        }
                    }
                }
                Err(e) => {
                    crate::ops::fail_op(&h, &format!("启动数字分身失败：{e}"));
                    crate::notify::notify(&h, "启动失败", &format!("{e}\n\n可在托盘「安装 / 修复」后重试。"));
                }
            }
        });
        return json_resp(StatusCode::OK, serde_json::json!({"ok": true, "message": "已开始启动数字分身"}));
    }
    if method == tauri::http::Method::GET && path == "/op-state" {
        // 向导窗口内嵌进度区轮询端点：返回 ops 全局状态的当前操作 JSON
        // （含步骤列表/当前步骤/日志/结果）。custom protocol 无 Tauri IPC，
        // 前端用轮询取数（与 console.rs 的 /state 一致）。
        let body = serde_json::to_string(&crate::ops::current())
            .unwrap_or_else(|_| "null".to_string());
        log::info!("matrix-setup:// /op-state 返回（截断）：{}", crate::config::truncate_utf8(&body, 120));
        return Response::builder()
            .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
            .body(body.into_bytes())
            .unwrap_or_default();
    }
    if method == tauri::http::Method::POST && path == "/retry-install" {
        // 阶段一「安装运行环境」失败后的重试入口：复用 install_all（幂等，已装则跳过）。
        // 进度照旧内嵌于向导窗口（前端轮询 /op-state），不弹独立进度窗。
        let h = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            crate::ops::append_log(&h, "[开始] 重试安装运行环境…");
            match tauri::async_runtime::block_on(crate::install::install_all(&h)) {
                Ok(()) => {
                    // ⭐ 阶段一完成后的「安装桥」收口：install_all 不保证 matrix profile
                    // 的 dsh-matrix-agent 已装（它取决于服务端清单是否出现该键；且在
                    // 「dsh 已装未激活」分流路径下 install_all 会被跳过，只有骨架）。
                    // 这里显式按 matrix 清单补装一次（幂等，已装则跳过）——缺了它
                    // diagnostics.log 永远不会出现，wait_matrix_ready 必超时（2026-09-24 实测）。
                    let h2 = h.clone();
                    let r = tauri::async_runtime::block_on(async move {
                        crate::install::install_server_recommended(&h2, &load_cached()).await
                    });
                    match r {
                        Ok(()) => log::info!("安装运行环境 + matrix 推荐插件完成"),
                        Err(e) => log::warn!("补装 matrix 推荐插件未完成（可托盘重试）：{e}"),
                    }
                    crate::ops::append_log(&h, "运行环境已就绪，请继续");
                    crate::notify::notify(&h, "安装完成", "运行环境已就绪，请继续激活数字分身。");
                }
                Err(e) => {
                    crate::ops::fail_op(&h, &format!("安装仍未完成：{e}"));
                    log::error!("重试安装失败（向导内触发）：{e}");
                }
            }
        });
        return json_resp(StatusCode::OK, serde_json::json!({"ok": true, "message": "已开始重试安装"}));
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
        // ⭐ 必选岗位兜底（不信任前端）：秘书/前台是分身正常工作的基础能力（请示分级/
        // 上呈主人 + 访客接待/咨询分流），取消后分身不完整。无论前端提交什么清单，
        // 后端都强制合入这两个岗位，杜绝「前端被绕过 / 旧版客户端」漏掉。
        let jobs = enforce_mandatory_jobs(jobs);
        let default_job: String = body
            .get("defaultJob")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        // 花名册开关（独立能力，不挂岗位）：缺省 false（保持关闭，按分身显式开启）。
        let roster_enabled: bool = body
            .get("rosterEnabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        // 校验 defaultJob 若给值，必须属于已勾选的 jobs（或至少是合法岗位 id）
        if !default_job.is_empty() && !jobs.iter().any(|j| j == &default_job) {
            return json_resp(
                StatusCode::OK,
                serde_json::json!({"ok": false, "error": "默认岗位必须从已勾选的岗位里选"}),
            );
        }
        let path = settings_yaml_path(app, &cfg);
        // 落盘预装岗位清单（用户勾选的）+ 默认岗位 + 花名册开关
        let jobs_result = crate::env_defaults::apply_job_presets_to_file(&path, &jobs);
        let dj_result = crate::env_defaults::apply_default_job_to_file(&path, &default_job);
        // ⭐ 打通「选岗位 → 挂岗位」：默认岗位同时写入 agent-presets.default，
        // 使 dsh-bridge 的 worker 会话 agentSetup() 能读到用户选的默认岗位
        // （否则会回退 cordis.patch.yml 写死的 agentPreset，历史教训：写死 pm 导致崩溃）。
        let apd_result = crate::env_defaults::apply_agent_presets_default_to_file(&path, &default_job);
        let roster_result = crate::env_defaults::apply_roster_enabled_to_file(&path, roster_enabled);
        if let Err(e) = jobs_result {
            log::warn!("[jobs] 落盘预装岗位清单失败：{e}");
        }
        if let Err(e) = dj_result {
            log::warn!("[jobs] 落盘默认岗位失败：{e}");
        }
        if let Err(e) = apd_result {
            log::warn!("[jobs] 落盘 agent-presets.default 失败：{e}");
        }
        if let Err(e) = roster_result {
            log::warn!("[jobs] 落盘花名册开关失败：{e}");
        }
        log::info!(
            "[jobs] 已保存岗位设置：预装 {} 个（含必选秘书/前台），默认岗位 {}，花名册 {}",
            jobs.len(),
            if default_job.is_empty() { "（未指定）" } else { &default_job },
            if roster_enabled { "开启" } else { "关闭" }
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
        // ⚠️ 单窗口一贯流程：进度内嵌于向导窗口（前端轮询 /op-state），不再弹独立
        // 「操作进度」窗；手动配置提交后向导留在前台展示进度，不再关闭重开。
        let h = app.clone();
        tauri::async_runtime::spawn(async move {
            let _ = crate::matrix_setup::open_window(&h); // 聚焦向导（进度内嵌）
        });
        return json_resp(StatusCode::OK, serde_json::json!({"ok": true, "message": "已提交手动配置"}));
    }
    json_resp(StatusCode::NOT_FOUND, serde_json::json!({"ok": false, "error": "not found"}))
}

/// 向导表单初始数据（GET /state 返回；预置值 + 当前已填值）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct WizardState {
    pub status: String,
    /// 两阶段相位：`not-installed`（阶段一未完成，需安装）| `ready`（阶段一完成，等待领号）
    /// | `configured`（两阶段完成）。前端据此渲染「安装」或「领号」UI。
    pub phase: String,
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
    /// 当前已落盘的花名册开关（roster.rosterEnabled），缺省 false（关闭）。
    pub roster_enabled: bool,
    /// 最近一次自动激活的失败文案（无失败/未激活过 = 空串）。前端据此恢复按钮并提示。
    pub last_activation_error: String,
    /// 数字分身（matrix profile）当前是否已在运行（前端据此渲染「启动 / 运行中」）。
    pub launcher_running: bool,
    /// 账号三要素已写（分身已激活）但 matrix profile 进程尚未运行——「激活完成、
    /// 等待用户确认重启」的中间态。前端据此展示分身账号并停在重启确认区，而不是
    /// 误判成「还没激活」或「已全部完成」。增量字段，不影响既有前端。
    pub activated_not_launched: bool,
    /// 当前生效 profile 已装插件（name → version），供「安装完成」成果清单展示。
    /// 空 map = 尚未安装任何插件。仅增量字段，不影响既有前端（旧版忽略它）。
    pub installed_plugins: std::collections::HashMap<String, String>,
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

/// ⭐ 必选岗位（分身正常工作必需，不可取消）。
///
/// 秘书（请示分级/决策回传/上呈主人/转达话术）与前台（访客接待/咨询分流）是数字分身
/// 的基础人设能力：取消后分身不完整、无法正常工作。本函数在「保存岗位设置」时对用户
/// 提交的预装清单做兜底合入——无论前端是否已锁定（UI 置灰）、是否被绕过（旧客户端 /
/// 直接 curl），后端都保证这两个岗位在最终清单里。与 matrix_setup 内嵌 JS 的同名常量
/// （MANDATORY_JOBS）保持同步。
pub fn enforce_mandatory_jobs(mut jobs: Vec<String>) -> Vec<String> {
    const MANDATORY_JOBS: [&str; 2] = ["secretary", "reception"];
    for mj in MANDATORY_JOBS {
        if !jobs.iter().any(|j| j == mj) {
            jobs.push(mj.to_string());
        }
    }
    jobs
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

/// 读取当前 settings.yaml 里的 `roster.rosterEnabled`（花名册开关），缺省 false。
fn read_roster_enabled<R: TauriRuntime>(app: &TauriAppHandle<R>, cfg: &LauncherConfig) -> bool {
    let path = settings_yaml_path(app, cfg);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return false,
    };
    use serde_yaml::Value;
    let root: Value = match serde_yaml::from_str(&text) {
        Ok(v) => v,
        Err(_) => return false,
    };
    root.get("roster")
        .and_then(|s| s.as_mapping())
        .and_then(|m| m.get(Value::String("rosterEnabled".to_string())))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// 收集向导初始数据（读当前配置 + 预置）。
pub fn collect_state<R: TauriRuntime>(app: &TauriAppHandle<R>, cfg: &LauncherConfig) -> WizardState {
    let st = status(app, cfg);
    let (acc, _) = load_account(app, cfg);
    let (status_str, phase, missing) = match &st {
        MatrixStatus::Configured => ("configured".to_string(), "configured".to_string(), Vec::new()),
        MatrixStatus::NotInstalled => ("not-installed".to_string(), "not-installed".to_string(), Vec::new()),
        MatrixStatus::Unconfigured { missing } => ("unconfigured".to_string(), "ready".to_string(), missing.clone()),
        // 阶段一完成、账号未配 → 阶段二「领号」UI
        MatrixStatus::ReadyToActivate => ("ready-activate".to_string(), "ready".to_string(), Vec::new()),
    };
    WizardState {
        status: status_str,
        phase,
        missing,
        homeserver_url: acc.homeserver_url,
        user_id: acc.user_id,
        access_token_set: !acc.access_token.is_empty() && acc.access_token != PENDING_CONFIG,
        owner: acc.owner,
        manual_config_enabled: manual_config_enabled(&cfg),
        pending_plugins: pending_plugin_diff(app, cfg),
        job_presets: job_presets_from_sync(app, cfg),
        default_job: read_default_job(app, cfg),
        roster_enabled: read_roster_enabled(app, cfg),
        last_activation_error: LAST_ACTIVATION_ERROR
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default(),
        // 「数字分身已就绪」区块判断：matrix profile 是否正在运行
        launcher_running: crate::workflow::is_running()
            && crate::workflow::current_profile().as_deref() == Some(MATRIX_PROFILE),
        // 「已激活未重启」中间态：账号已写但进程未跑（等待用户确认重启）
        activated_not_launched: activated_not_launched(app, cfg),
        // 「安装完成」成果清单：当前生效 profile 已装插件（name → version）。
        installed_plugins: crate::sync::installed_plugins_current_profile_with_versions(app, cfg),
    }
}

/// 向导 HTML（内嵌，无需前端构建；仿 console.rs）。
/// HTML 已抽为独立静态资源（src-tauri/embedded-ui/matrix-setup.html），
/// 双轨加载：服务端下发版优先（同步拉取缓存在 app_data/ui-bundle/），离线回落内置。
pub fn wizard_html<R: Runtime>(app: &AppHandle<R>) -> String {
    crate::embedded::matrix_setup_html(app)
}
