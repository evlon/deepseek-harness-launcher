//! 数字分身「自动激活」：授权码 + PKCE + 本地回调（P3）。
//!
//! 流程（契约见 docs/数字分身激活-P3-契约与实现.md）：
//!   ① 起本地回调 http://127.0.0.1:45813/callback（tiny_http，单次）
//!   ② 生成 state（CSRF）+ PKCE verifier/challenge（getrandom + sha2 S256）
//!   ③ 打开系统浏览器 → Keycloak 授权端点
//!   ④ 用户 Keycloak 登录（已有 SSO 会话则秒过）
//!   ⑤ Keycloak 302 → 本地回调 ?code=...&state=...
//!   ⑥ 校验 state，用 code+verifier POST token 端点换 id_token
//!   ⑦ POST matrix-account-manager /activate { idToken } → 分身 access_token
//!   ⑧ 写 settings.yaml（复用 matrix_setup::write_account）
//!
//! 安全要点：
//!   · state 与 PKCE verifier 用 getrandom（密码学强度随机），非弱伪随机
//!   · 回调仅绑定 127.0.0.1（不暴露局域网）
//!   · 只处理一次回调（单次激活流程，防重放）
//!   · id_token / access_token 不落盘、不写日志

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Runtime};
use tauri_plugin_opener::OpenerExt;

use crate::config::*;
use crate::matrix_setup::MatrixAccount;

/// 激活端点的默认回调端口（Keycloak redirect_uri 精确匹配；45813~45815 已注册）。
const CALLBACK_BASE_PORT: u16 = 45813;
/// 回调端口顺延上限（端口被占时 +1 重试）。
const CALLBACK_PORT_RANGE: u16 = 3;

/// 激活相关地址（从 settings.yaml `matrix-activation` namespace 读，代码兜底）。
#[derive(Debug, Clone)]
pub struct ActivationConfig {
    /// Keycloak issuer（如 https://auth.ict.cmcc/realms/himarket）
    pub issuer: String,
    /// twin client 的 clientId（aud 校验 + 授权请求）
    pub client_id: String,
    /// client secret（confidential client 换 token 用）
    pub client_secret: String,
    /// matrix-account-manager /activate 端点完整 URL
    pub activate_endpoint: String,
    /// 分身 homeserver（写 settings.yaml 的 homeserverUrl）
    pub homeserver_url: String,
}

impl Default for ActivationConfig {
    fn default() -> Self {
        ActivationConfig {
            issuer: "https://auth.ict.cmcc/realms/himarket".to_string(),
            client_id: "matrix-twin-activation".to_string(),
            client_secret: String::new(),
            activate_endpoint: "http://im.ai.ict.cmcc/_matrix/activate".to_string(),
            homeserver_url: "https://im-ipm.ict.cmcc".to_string(),
        }
    }
}

/// 从 settings.yaml 的 `matrix-activation` namespace 读取激活配置，缺省回退内置默认。
///
/// 下发优先级（与 env_defaults 一致）：
///   settings.yaml（服务端 envDefaults 下发）> 代码内置默认。
/// client_secret 属敏感凭据，**不**经 settings 明文下发，从环境变量
/// `DSH_TWIN_CLIENT_SECRET` 读（部署/测试时由管理员注入），缺省为空则
/// 授权码换 token 时不带 secret（public client 模式，Keycloak 仍可换）。
fn read_activation_config<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> ActivationConfig {
    let mut out = ActivationConfig::default();
    let path = crate::matrix_setup::settings_yaml_path(app, cfg);
    let ns = read_yaml_ns_str(&path, "matrix-activation");
    if let Some(ns) = ns {
        if let Some(v) = ns.get("keycloakIssuer").filter(|s| !s.is_empty()) {
            out.issuer = v.clone();
        }
        if let Some(v) = ns.get("clientId").filter(|s| !s.is_empty()) {
            out.client_id = v.clone();
        }
        if let Some(v) = ns.get("activateEndpoint").filter(|s| !s.is_empty()) {
            out.activate_endpoint = v.clone();
        }
        if let Some(v) = ns.get("homeserverUrl").filter(|s| !s.is_empty()) {
            out.homeserver_url = v.clone();
        }
    }
    // client_secret 从环境变量读（敏感，不落 settings）
    out.client_secret = std::env::var("DSH_TWIN_CLIENT_SECRET").unwrap_or_default();
    out
}

/// 读 settings.yaml 某 namespace 下的字符串键值表（纯读取，不影响文件）。
/// 文件不存在/解析失败/namespace 缺失 → None。
fn read_yaml_ns_str(path: &Path, ns: &str) -> Option<std::collections::BTreeMap<String, String>> {
    use serde_yaml::Value;
    let text = std::fs::read_to_string(path).ok()?;
    let root: Value = serde_yaml::from_str(&text).ok()?;
    let section = root.get(ns)?;
    let mapping = section.as_mapping()?;
    let mut out = std::collections::BTreeMap::new();
    for (k, v) in mapping {
        if let (Some(key), Some(val)) = (k.as_str(), v.as_str()) {
            out.insert(key.to_string(), val.to_string());
        }
    }
    Some(out)
}

/// 生成密码学强度的随机 URL-safe 字符串（PKCE verifier / state）。
fn secure_random_urlsafe(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    getrandom::getrandom(&mut bytes).expect("getrandom 失败（系统熵源不可用）");
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes)
}

/// PKCE S256：verifier → challenge（base64url(sha256(verifier))）。
fn pkce_challenge(verifier: &str) -> String {
    use sha2::{Digest, Sha256};
    use base64::Engine;
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&digest)
}

/// 激活结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ActivationResult {
    pub ok: bool,
    pub message: String,
    /// 激活成功时的分身账号（@ai-<uid>:server）
    pub user_id: String,
    /// 激活成功时的分身 access_token（供写 settings.yaml）
    pub access_token: String,
    /// 是否新建（vs 找回）
    pub created: bool,
}

/// 运行中激活流程状态（单次激活）。
/// 状态由 [`CallbackServer`] 承载（state/verifier/result/shutdown），此注释保留说明。

/// 打开系统浏览器（复用 tauri-plugin-opener）。
fn open_browser<R: Runtime>(app: &AppHandle<R>, url: &str) -> Result<(), String> {
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| format!("打开浏览器失败：{e}"))?;
    Ok(())
}

/// 运行「自动激活」完整流程（阻塞，供向导后台线程调用）。
///
/// 返回 ActivationResult；ok=false 时 message 携带可读错误。
pub fn run_activation<R: Runtime>(app: &AppHandle<R>) -> ActivationResult {
    let cfg = load_cached();
    let acfg = read_activation_config(app, &cfg);

    // 预检：client 配置是否可用
    if acfg.issuer.is_empty() || acfg.client_id.is_empty() || acfg.activate_endpoint.is_empty() {
        return ActivationResult {
            ok: false,
            message: "激活配置缺失（issuer/clientId/activateEndpoint），请联系管理员下发".to_string(),
            user_id: String::new(),
            access_token: String::new(),
            created: false,
        };
    }

    // ① 起本地回调服务（127.0.0.1:45813，端口冲突顺延）
    let mut callback = match start_callback_server() {
        Ok(c) => c,
        Err(e) => {
            return ActivationResult {
                ok: false,
                message: format!("启动本地回调失败：{e}"),
                user_id: String::new(),
                access_token: String::new(),
                created: false,
            };
        }
    };

    // ② 生成 state + PKCE
    let state = callback.state.clone();
    let verifier = callback.verifier.clone();
    let challenge = pkce_challenge(&verifier);
    let redirect_uri = format!("http://127.0.0.1:{}/callback", callback.port);

    // ③ 构造授权 URL 并打开浏览器
    let auth_url = build_auth_url(&acfg, &redirect_uri, &state, &challenge);
    log::info!("[activation] 打开浏览器授权：{}", auth_url);
    if let Err(e) = open_browser(app, &auth_url) {
        stop_callback_server(&mut callback);
        return ActivationResult {
            ok: false,
            message: e,
            user_id: String::new(),
            access_token: String::new(),
            created: false,
        };
    }

    // ④ 等待回调（超时 120s，用户登录 + 跳转）
    let code = match wait_for_callback(&callback, 120) {
        Ok(c) => c,
        Err(e) => {
            stop_callback_server(&mut callback);
            return ActivationResult {
                ok: false,
                message: e,
                user_id: String::new(),
                access_token: String::new(),
                created: false,
            };
        }
    };
    stop_callback_server(&mut callback);

    // ⑤ 换 token（code + verifier + client_secret）
    let id_token = match exchange_code(&acfg, &redirect_uri, &code, &verifier) {
        Ok(t) => t,
        Err(e) => {
            return ActivationResult {
                ok: false,
                message: format!("换取身份令牌失败：{e}"),
                user_id: String::new(),
                access_token: String::new(),
                created: false,
            };
        }
    };

    // ⑥ 调 /activate 建号
    let (user_id, access_token, created) = match call_activate(&acfg, &id_token) {
        Ok(v) => v,
        Err(e) => {
            return ActivationResult {
                ok: false,
                message: format!("激活数字分身失败：{e}"),
                user_id: String::new(),
                access_token: String::new(),
                created: false,
            };
        }
    };

    // ⑦ 写 settings.yaml
    let acc = MatrixAccount {
        homeserver_url: acfg.homeserver_url.clone(),
        user_id: user_id.clone(),
        access_token: access_token.clone(),
        owner: owner_from_user_id(&user_id),
    };
    if let Err(e) = crate::matrix_setup::write_account(app, &cfg, &acc) {
        return ActivationResult {
            ok: false,
            message: format!("分身已激活但写入配置失败：{e}"),
            user_id,
            access_token,
            created,
        };
    }

    ActivationResult {
        ok: true,
        message: format!("数字分身已激活：{user_id}（{}）", if created { "新建" } else { "已找回" }),
        user_id,
        access_token,
        created,
    }
}

/// 从分身 userId（@ai-xxx:server）推导 owner（@xxx:server，真人账号）。
fn owner_from_user_id(user_id: &str) -> String {
    // @ai-niukunliang:im.ai.ict.cmcc → @niukunliang:im.ai.ict.cmcc
    if let Some(rest) = user_id.strip_prefix("@ai-") {
        return format!("@{rest}");
    }
    user_id.to_string()
}

/// 构造 Keycloak 授权 URL。
fn build_auth_url(acfg: &ActivationConfig, redirect_uri: &str, state: &str, challenge: &str) -> String {
    let mut params: Vec<(String, String)> = vec![
        ("client_id".into(), acfg.client_id.clone()),
        ("redirect_uri".into(), redirect_uri.to_string()),
        ("response_type".into(), "code".into()),
        ("scope".into(), "openid".into()),
        ("state".into(), state.to_string()),
        ("code_challenge".into(), challenge.to_string()),
        ("code_challenge_method".into(), "S256".into()),
    ];
    // 让参数顺序稳定（可测试）
    params.sort_by(|a, b| a.0.cmp(&b.0));
    let qs = params
        .iter()
        .map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{}/protocol/openid-connect/auth?{}", acfg.issuer.trim_end_matches('/'), qs)
}

/// 简单 URL 编码（query 参数用；用 url crate 更规范，但这里手工足够）。
fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{:02X}", b));
            }
        }
    }
    out
}

// ---------- 本地回调服务 ----------

/// 本地回调服务句柄。
struct CallbackServer {
    port: u16,
    state: String,
    verifier: String,
    result: Arc<(Mutex<Option<(String, String)>>, std::sync::Condvar)>,
    shutdown: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// 启动本地回调服务（绑定 127.0.0.1，端口冲突顺延）。
fn start_callback_server() -> Result<CallbackServer, String> {
    let state = secure_random_urlsafe(32);
    let verifier = secure_random_urlsafe(48);
    let result: Arc<(Mutex<Option<(String, String)>>, std::sync::Condvar)> =
        Arc::new((Mutex::new(None), std::sync::Condvar::new()));
    let shutdown = Arc::new(AtomicBool::new(false));

    let mut port = CALLBACK_BASE_PORT;
    let server = loop {
        match tiny_http::Server::http(("127.0.0.1", port)) {
            Ok(s) => break s,
            Err(_) => {
                if port >= CALLBACK_BASE_PORT + CALLBACK_PORT_RANGE {
                    return Err(format!(
                        "端口 {CALLBACK_BASE_PORT}~{} 均被占用",
                        CALLBACK_BASE_PORT + CALLBACK_PORT_RANGE
                    ));
                }
                port += 1;
            }
        }
    };

    let result_clone = result.clone();
    let shutdown_clone = shutdown.clone();
    let state_clone = state.clone();
    let thread = std::thread::spawn(move || {
        log::info!("[activation] 本地回调服务已启动：http://127.0.0.1:{port}/callback");
        // 只处理一次请求（单次激活），处理完即退出循环
        for request in server.incoming_requests() {
            if shutdown_clone.load(Ordering::Relaxed) {
                break;
            }
            let url = request.url().to_string();
            log::info!("[activation] 收到回调：{}", url);
            // 解析 query：?code=...&state=...
            let (code, got_state) = parse_callback_query(&url);
            // 响应：成功/失败都给一个简单页面（浏览器可见）
            let body = if code.is_empty() {
                "<html><body><h3>激活失败</h3><p>未收到授权码，请关闭此页回到启动器。</p></body></html>"
            } else {
                "<html><body><h3>授权成功</h3><p>可关闭此页，回到启动器完成激活。</p></body></html>"
            };
            let _ = request.respond(
                tiny_http::Response::from_string(body.to_string())
                    .with_header(
                        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
                            .unwrap(),
                    ),
            );
            // 存结果并唤醒等待方
            {
                let (lock, cvar) = &*result_clone;
                let mut guard = lock.lock().unwrap();
                *guard = Some((code, got_state));
                cvar.notify_all();
            }
            break; // 单次激活：处理一个回调即停
        }
        let _ = state_clone;
    });

    Ok(CallbackServer {
        port,
        state,
        verifier,
        result,
        shutdown,
        thread: Some(thread),
    })
}

/// 解析回调 URL 的 query（?code=...&state=...），返回 (code, state)。
fn parse_callback_query(url: &str) -> (String, String) {
    let query = url.split('?').nth(1).unwrap_or("");
    let mut code = String::new();
    let mut state = String::new();
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            let v = url_decode(v);
            match k {
                "code" => code = v,
                "state" => state = v,
                _ => {}
            }
        }
    }
    (code, state)
}

/// 简单 URL 解码（percent-decode）。
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// 等待回调（阻塞，超时返回错误）。
fn wait_for_callback(cb: &CallbackServer, timeout_secs: u64) -> Result<String, String> {
    let (lock, cvar) = &*cb.result;
    let mut guard = lock.lock().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        if let Some((code, got_state)) = guard.as_ref() {
            if code.is_empty() {
                return Err("授权失败：浏览器返回错误（未取得授权码）".to_string());
            }
            if *got_state != cb.state {
                return Err("安全校验失败：state 不匹配（可能存在 CSRF 攻击）".to_string());
            }
            return Ok(code.clone());
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return Err(format!("等待授权超时（{}s），请重试", timeout_secs));
        }
        let remaining = deadline - now;
        let (guard2, _) = cvar
            .wait_timeout(guard, remaining)
            .map_err(|e| format!("等待回调异常：{e}"))?;
        guard = guard2;
    }
}

/// 停止本地回调服务。
fn stop_callback_server(cb: &mut CallbackServer) {
    cb.shutdown.store(true, Ordering::Relaxed);
    // 主动连一下唤醒 accept 循环（否则 incoming_requests 可能阻塞）
    let _ = std::net::TcpStream::connect(("127.0.0.1", cb.port));
    if let Some(t) = cb.thread.take() {
        let _ = t.join();
    }
    log::info!("[activation] 本地回调服务已停止");
}

// ---------- token 交换 + 激活调用 ----------

/// 用授权码换 id_token（POST token 端点）。
fn exchange_code(
    acfg: &ActivationConfig,
    redirect_uri: &str,
    code: &str,
    verifier: &str,
) -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("dsh-harness-launcher-activation")
        .connect_timeout(std::time::Duration::from_secs(8))
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;

    let mut params: Vec<(String, String)> = vec![
        ("grant_type".into(), "authorization_code".into()),
        ("code".into(), code.to_string()),
        ("redirect_uri".into(), redirect_uri.to_string()),
        ("client_id".into(), acfg.client_id.clone()),
        ("code_verifier".into(), verifier.to_string()),
    ];
    if !acfg.client_secret.is_empty() {
        params.push(("client_secret".into(), acfg.client_secret.clone()));
    }

    let token_url = format!("{}/protocol/openid-connect/token", acfg.issuer.trim_end_matches('/'));
    let resp = client
        .post(&token_url)
        .form(&params)
        .send()
        .map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let text = resp.text().unwrap_or_default();
    if status != 200 {
        return Err(format!("HTTP {status}: {}", text.chars().take(200).collect::<String>()));
    }
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let id_token = json
        .get("id_token")
        .and_then(|v| v.as_str())
        .ok_or("响应中没有 id_token")?
        .to_string();
    if id_token.is_empty() {
        return Err("id_token 为空".to_string());
    }
    Ok(id_token)
}

/// 调 matrix-account-manager /activate，返回 (user_id, access_token, created)。
fn call_activate(acfg: &ActivationConfig, id_token: &str) -> Result<(String, String, bool), String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("dsh-harness-launcher-activation")
        .connect_timeout(std::time::Duration::from_secs(8))
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;

    let resp = client
        .post(&acfg.activate_endpoint)
        .json(&serde_json::json!({ "idToken": id_token }))
        .send()
        .map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let text = resp.text().unwrap_or_default();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    if status != 200 {
        let err = json
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or(&text)
            .to_string();
        return Err(format!("激活服务返回 HTTP {status}: {}", err.chars().take(200).collect::<String>()));
    }
    let user_id = json
        .get("user_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let access_token = json
        .get("access_token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let created = json.get("created").and_then(|v| v.as_bool()).unwrap_or(false);
    if user_id.is_empty() || access_token.is_empty() {
        return Err("激活服务响应缺少 user_id 或 access_token".to_string());
    }
    Ok((user_id, access_token, created))
}

// ---------- 测试 ----------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_base64url_sha256() {
        // RFC 7636 附录 B 的标准测试向量
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = pkce_challenge(verifier);
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn url_encode_handles_special_chars() {
        assert_eq!(url_encode("a b"), "a%20b");
        assert_eq!(url_encode("a/b"), "a%2Fb");
        assert_eq!(url_encode("abc123-_.~"), "abc123-_.~");
    }

    #[test]
    fn url_decode_handles_percent_and_plus() {
        assert_eq!(url_decode("a%20b"), "a b");
        assert_eq!(url_decode("a+b"), "a b");
        assert_eq!(url_decode("abc"), "abc");
    }

    #[test]
    fn parse_callback_query_extracts_code_and_state() {
        let url = "/callback?code=abc123&state=xyz789";
        let (code, state) = parse_callback_query(url);
        assert_eq!(code, "abc123");
        assert_eq!(state, "xyz789");
    }

    #[test]
    fn owner_from_user_id_strips_ai_prefix() {
        assert_eq!(
            owner_from_user_id("@ai-niukunliang:im.ai.ict.cmcc"),
            "@niukunliang:im.ai.ict.cmcc"
        );
        assert_eq!(owner_from_user_id("@noprefix:server"), "@noprefix:server");
    }

    #[test]
    fn secure_random_urlsafe_generates_unique_values() {
        let a = secure_random_urlsafe(32);
        let b = secure_random_urlsafe(32);
        assert_eq!(a.len(), 43); // 32 bytes base64url no pad
        assert_ne!(a, b);
        // 无 base64 填充字符、无 + /
        assert!(!a.contains('=') && !a.contains('+') && !a.contains('/'));
    }
}
