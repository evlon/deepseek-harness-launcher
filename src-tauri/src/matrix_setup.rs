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
use tauri::{AppHandle, Runtime};

use crate::config::*;

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
                    // 配置已由 run_activation 写入 settings.yaml，这里只需重启 + 等连接
                    let cfg_now = load_cached();
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
                        }
                    }
                }
                r => {
                    crate::ops::fail_op(&h, &r.message);
                    crate::notify::notify(&h, "自动激活失败", &r.message);
                }
            }
        });
        // 弹进度窗口 + 关闭向导（与 /submit 一致）
        let h = app.clone();
        tauri::async_runtime::spawn(async move {
            std::thread::sleep(std::time::Duration::from_millis(400));
            let _ = crate::console::open_console(&h);
            std::thread::sleep(std::time::Duration::from_millis(300));
            if let Some(win) = h.get_webview_window("matrix-setup") {
                let _ = win.close();
            }
        });
        return json_resp(StatusCode::OK, serde_json::json!({"ok": true, "message": "已开始自动激活"}));
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
                    let running = crate::workflow::is_running();
                    let cur_profile = crate::workflow::current_profile();
                    crate::ops::mark_step_running(&h, 1);
                    if running && cur_profile.as_deref() == Some(MATRIX_PROFILE) {
                        crate::workflow::stop();
                        std::thread::sleep(std::time::Duration::from_millis(800));
                    }
                    match crate::workflow::launch_with_profile(&h, MATRIX_PROFILE) {
                        Ok(port) => {
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
}

/// 计算订阅/安装清单差异：服务端推荐清单 vs 本地已装清单。
/// 返回 [{name, installed, latest, action}]，action ∈ install | update。
fn pending_plugin_diff<R: TauriRuntime>(app: &TauriAppHandle<R>, cfg: &LauncherConfig) -> Vec<serde_json::Value> {
    let state = crate::sync::load_state(app, cfg);
    let Some(recommended) = state.cached_config.as_ref().map(|c| c.plugins.clone()) else {
        return Vec::new();
    };
    let installed = crate::sync::installed_plugins_current_profile_with_versions(app, cfg);
    crate::sync::pending_with_updates(&recommended, &installed, &state.plugin_latest_versions)
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
  }).catch(()=>{});

  // 自动激活：调本机服务，打开浏览器授权（窗口随后自动关闭，进度走操作窗口）
  $("activateBtn").onclick=async()=>{
    const st=$("activateStatus"); st.className="status info"; st.textContent="正在打开浏览器授权…";
    $("activateBtn").disabled=true;
    try{
      const r=await fetch("http://matrix-setup.localhost/activate",{method:"POST",headers:{"Content-Type":"application/json"},body:"{}"});
      const j=await r.json();
      if(j.ok){
        st.innerHTML='<span class="ok">✓ 已启动！浏览器即将打开，请完成公司 SSO 登录。本窗口会自动关闭，进度在操作窗口显示。</span>';
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
