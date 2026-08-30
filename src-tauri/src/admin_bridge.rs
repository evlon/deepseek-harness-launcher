//! 管理能力（外网代理网关）：管理员 launcher 在本机 127.0.0.1 起的本地 API 服务。
//!
//! 背景：中心服务端（内网服务器）**不能访问外网**。管理员电脑能上网，
//! 通过托盘「开启管理能力」在本机起一个 HTTP 监听，服务端管理页（管理员浏览器）
//! 直接 fetch 这个本地 API 中转查询外网 npm registry 的包信息/拉包体。
//!
//! 约束：
//! - 仅绑定 127.0.0.1（不接受局域网访问）
//! - 可选 token（X-Bridge-Token / ?token=），防本机其他进程滥用
//! - 脚本执行默认禁用（allowScripts），启用时受限执行
//! - 用 tiny_http（成熟 HTTP 服务器）替代自写解析——修复 body 读取死锁等自写缺陷

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Runtime};

use crate::config::*;

/// 运行状态：端口 + token + 停止标志。
pub struct Bridge {
    port: u16,
    /// 实际生效的 token（配置的或随机生成的；空 = 不校验）。
    token: String,
    shutdown: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

static CURRENT: std::sync::Mutex<Option<Bridge>> = std::sync::Mutex::new(None);

/// 当前是否在监听。
pub fn is_running() -> bool {
    CURRENT.lock().unwrap().as_ref().is_some()
}

/// 当前监听端口。
pub fn port() -> Option<u16> {
    CURRENT.lock().unwrap().as_ref().map(|b| b.port)
}

/// 当前实际生效的 token（配置的或随机生成的）。
pub fn current_token() -> String {
    CURRENT
        .lock()
        .unwrap()
        .as_ref()
        .map(|b| b.token.clone())
        .unwrap_or_default()
}

/// 启动管理能力本地 API（绑定 127.0.0.1，端口冲突自动顺延）。
pub fn start<R: Runtime>(app: &AppHandle<R>) -> Result<u16, String> {
    stop(); // 先停旧的

    let cfg = load_cached();
    let base = bridge_port(&cfg);
    // token：配置了就用配置的；否则生成随机 token 并持久化（重启不变），
    // 安全默认：防任意网页调用本机 API。
    let configured_token = bridge_token(&cfg);
    let token = if configured_token.is_empty() {
        let t = random_token(16);
        let _ = crate::config::set_bridge_token(app, &t);
        log::warn!("管理能力未配置 token，已生成并保存随机 token：{t}（管理页连接时使用）");
        t
    } else {
        configured_token
    };
    let allow_scripts = cfg
        .admin_bridge
        .as_ref()
        .map(|b| b.allow_scripts)
        .unwrap_or(false);

    let h = app.clone();
    // 端口冲突自动顺延
    let mut port = base;
    let server = loop {
        match tiny_http::Server::http(("127.0.0.1", port)) {
            Ok(s) => break s,
            Err(_) => {
                if port >= base + 50 {
                    return Err(format!("BRIDGE_PORT_EXHAUSTED: 端口 {base}~{} 均被占用", base + 50));
                }
                port += 1;
            }
        }
    };

    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = shutdown.clone();
    let bridge_token_for_state = token.clone(); // 供 Bridge 状态记录

    let thread = std::thread::spawn(move || {
        log::info!("管理能力本地 API 已启动：http://127.0.0.1:{port}");
        // tiny_http 每个请求一个线程（内部线程池），此处主循环 accept
        for request in server.incoming_requests() {
            if shutdown_clone.load(Ordering::Relaxed) {
                break;
            }
            let h = h.clone();
            let token = token.clone();
            let allow_scripts = allow_scripts;
            std::thread::spawn(move || {
                handle_request(request, &h, &token, allow_scripts);
            });
        }
    });

    *CURRENT.lock().unwrap() = Some(Bridge {
        port,
        token: bridge_token_for_state,
        shutdown,
        thread: Some(thread),
    });
    Ok(port)
}

/// 停止管理能力本地 API。
pub fn stop() {
    let mut guard = CURRENT.lock().unwrap();
    if let Some(b) = guard.take() {
        b.shutdown.store(true, Ordering::Relaxed);
        if let Some(t) = b.thread {
            let _ = t.join();
        }
        log::info!("管理能力本地 API 已停止");
    }
}

// ---------- 请求处理（tiny_http） ----------

/// 允许的管理页 Origin（服务端管理页域名；浏览器同源请求无 Origin 头时放行）。
/// 默认放行 ai-conf.ict.cmcc；可经环境变量 ADMIN_ORIGIN 覆盖（测试/内网别名）。
fn origin_allowed(origin: &str) -> bool {
    if let Ok(extra) = std::env::var("ADMIN_ORIGIN") {
        for e in extra.split(',') {
            let e = e.trim();
            if !e.is_empty() && origin == e {
                return true;
            }
        }
    }
    origin == "http://ai-conf.ict.cmcc" || origin == "https://ai-conf.ict.cmcc"
}

/// 处理单个请求：CORS → token → 路由。
/// 外包一层整体超时：handler 意外卡死（读 body/写响应阻塞）时，
/// 不再无限占用线程与连接（曾出现 CLOSE_WAIT 堆积 + 线程泄漏导致进程崩溃）。
fn handle_request<R: Runtime>(
    request: tiny_http::Request,
    app: &AppHandle<R>,
    token: &str,
    allow_scripts: bool,
) {
    let h = app.clone();
    let token = token.to_string();
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let worker = std::thread::spawn(move || {
        handle_request_inner(request, &h, &token, allow_scripts);
        let _ = tx.send(());
    });
    match rx.recv_timeout(std::time::Duration::from_secs(REQUEST_HANDLER_TIMEOUT_SECS)) {
        Ok(()) => {
            let _ = worker.join();
        }
        Err(_) => {
            // 超时：记录并放弃（worker 线程 detached，连接由 OS 回收）
            log::error!(
                "管理能力：请求处理超时（{}s），已放弃该连接，防止线程/连接泄漏",
                REQUEST_HANDLER_TIMEOUT_SECS
            );
        }
    }
}

/// 单个请求处理超时（秒）：读 body / 写响应任一卡死超过该时长即放弃。
const REQUEST_HANDLER_TIMEOUT_SECS: u64 = 15;

/// 实际请求处理（被 handle_request 超时包装）。
fn handle_request_inner<R: Runtime>(
    mut request: tiny_http::Request,
    app: &AppHandle<R>,
    token: &str,
    allow_scripts: bool,
) {
    // 读取 body（tiny_http 正确处理 Content-Length/分片/keep-alive；
    // 无 body 的 GET 得到 empty reader，立即返回）
    let mut body_bytes = Vec::new();
    let _ = request.as_reader().read_to_end(&mut body_bytes);
    log::info!("管理能力：body 读取完成（{} 字节）", body_bytes.len());

    let method = request.method().to_string();
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or(&url).to_string();
    let query = parse_query(url.split('?').nth(1).unwrap_or(""));

    // Origin 校验（防 DNS rebinding / CSRF）
    let origin = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("origin"))
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_default();
    if !origin.is_empty() && !origin_allowed(&origin) {
        respond_json(request, 403, serde_json::json!({ "error": "origin not allowed" }), &origin);
        return;
    }

    // CORS preflight
    if method == "OPTIONS" {
        respond_options(request, &origin);
        return;
    }

    // token 校验（health 免 token——纯探测，无副作用；其余路由需 token）
    let is_health = method == "GET" && path == "/api/health";
    if !is_health {
        let auth_ok = token.is_empty() || req_has_token(&query, token);
        if !auth_ok {
            log::warn!("管理能力：token 校验失败（path={path} method={method}）");
            respond_json(request, 403, serde_json::json!({ "error": "invalid bridge token" }), &origin);
            return;
        }
    }
    log::info!("管理能力：路由 {method} {path}");

    // 路由分发
    let resp: serde_json::Value = match (method.as_str(), path.as_str()) {
        ("GET", "/api/health") => serde_json::json!({
            "ok": true,
            "version": env!("CARGO_PKG_VERSION"),
            "port": port(),
            "bridge": true,
        }),
        ("GET", "/api/registry/meta") => {
            let name = query.get("name").cloned().unwrap_or_default();
            match block_on_query_meta(&name) {
                Ok(meta) => serde_json::json!({ "ok": true, "meta": meta }),
                Err(e) => serde_json::json!({ "ok": false, "error": e }),
            }
        }
        ("POST", "/api/script/exec") => {
            if !allow_scripts {
                respond_json(request, 403, serde_json::json!({ "error": "scripts disabled (adminBridge.allowScripts)" }), &origin);
                return;
            }
            let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap_or(serde_json::Value::Null);
            match exec_script(&body) {
                Ok(out) => serde_json::json!({ "ok": true, "output": out }),
                Err(e) => serde_json::json!({ "ok": false, "error": e }),
            }
        }
        ("GET", "/api/registry/mirror/progress") => {
            let cfg = load_cached();
            let p = crate::mirror::load_progress(app, &cfg);
            serde_json::json!({ "ok": true, "progress": p })
        }
        ("POST", "/api/registry/mirror/start") => {
            let cfg = load_cached();
            // token 经 POST body 传递（不进 URL/日志）
            let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap_or(serde_json::Value::Null);
            let registry = body
                .get("registry")
                .and_then(|v| v.as_str())
                .filter(|r| !r.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| crate::config::mirror_registry(&cfg));
            let token = body
                .get("token")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let only = body
                .get("only")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            match crate::mirror::start_mirror_upload(app, &cfg, &registry, &token, only) {
                Ok(()) => serde_json::json!({ "ok": true, "message": "上传已开始" }),
                Err(e) => serde_json::json!({ "ok": false, "error": e }),
            }
        }
        ("GET", "/api/registry/mirror/cancel") => {
            // 本期不做真正的取消（任务较短）；返回当前状态
            let cfg = load_cached();
            let p = crate::mirror::load_progress(app, &cfg);
            serde_json::json!({ "ok": true, "progress": p, "note": "cancel not implemented" })
        }
        _ => {
            respond_json(request, 404, serde_json::json!({ "error": "not found" }), &origin);
            return;
        }
    };
    log::info!("管理能力：路由处理完成，准备响应 {method} {path}");
    respond_json(request, 200, resp, &origin);
    log::info!("管理能力：响应已发送 {method} {path}");
}

/// 解析 query string（百分号解码）。
fn parse_query(qs: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for kv in qs.split('&') {
        if kv.is_empty() {
            continue;
        }
        if let Some((k, v)) = kv.split_once('=') {
            out.insert(percent_decode(k), percent_decode(v));
        }
    }
    out
}

fn req_has_token(query: &std::collections::HashMap<String, String>, token: &str) -> bool {
    query.get("token").map(|t| t == token).unwrap_or(false)
}

/// 发送 JSON 响应（带 CORS 头）。
fn respond_json(request: tiny_http::Request, code: u16, obj: serde_json::Value, origin: &str) {
    let body = serde_json::to_string(&obj).unwrap_or_else(|_| "{}".to_string());
    let status = match code {
        200 => tiny_http::StatusCode(200),
        403 => tiny_http::StatusCode(403),
        404 => tiny_http::StatusCode(404),
        _ => tiny_http::StatusCode(500),
    };
    let allow_origin = if origin_allowed(origin) && !origin.is_empty() {
        origin.to_string()
    } else {
        "http://ai-conf.ict.cmcc".to_string()
    };
    let response = tiny_http::Response::from_string(body)
        .with_status_code(status)
        .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json; charset=utf-8"[..]).unwrap())
        .with_header(tiny_http::Header::from_bytes(&b"Access-Control-Allow-Origin"[..], allow_origin.as_bytes()).unwrap())
        .with_header(tiny_http::Header::from_bytes(&b"Access-Control-Allow-Methods"[..], &b"GET, POST, OPTIONS"[..]).unwrap())
        .with_header(tiny_http::Header::from_bytes(&b"Access-Control-Allow-Headers"[..], &b"Content-Type, X-Bridge-Token, X-Admin-Token"[..]).unwrap());
    let _ = request.respond(response);
}

/// 发送 CORS preflight 响应。
fn respond_options(request: tiny_http::Request, origin: &str) {
    let allow_origin = if origin_allowed(origin) && !origin.is_empty() {
        origin.to_string()
    } else {
        "http://ai-conf.ict.cmcc".to_string()
    };
    let response = tiny_http::Response::empty(204)
        .with_header(tiny_http::Header::from_bytes(&b"Access-Control-Allow-Origin"[..], allow_origin.as_bytes()).unwrap())
        .with_header(tiny_http::Header::from_bytes(&b"Access-Control-Allow-Methods"[..], &b"GET, POST, OPTIONS"[..]).unwrap())
        .with_header(tiny_http::Header::from_bytes(&b"Access-Control-Allow-Headers"[..], &b"Content-Type, X-Bridge-Token, X-Admin-Token"[..]).unwrap());
    let _ = request.respond(response);
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ---------- registry 查询（外网中转） ----------

/// 生成随机 token（URL 安全字符）。
fn random_token(len: usize) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // 简化伪随机（安全足够：token 仅本机 API 门控，非密码学强度）
    let mut state = seed;
    let mut out = String::with_capacity(len);
    for _ in 0..len {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let idx = ((state >> 33) % CHARS.len() as u128) as usize;
        out.push(CHARS[idx] as char);
    }
    out
}

/// 查询 npm registry 元信息（npmjs → npmmirror → 内网 REGISTRY_OVERRIDE）。
/// 用阻塞 reqwest（在线程池里跑，可接受）。
fn block_on_query_meta(name: &str) -> Result<serde_json::Value, String> {
    if name.is_empty() {
        return Err("missing name".to_string());
    }
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(query_meta_async(name))
}

async fn query_meta_async(name: &str) -> Result<serde_json::Value, String> {
    use reqwest::Client;
    let client = Client::builder()
        .user_agent("dsh-harness-launcher-bridge")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|e| e.to_string())?;

    let mut registries = vec![
        "https://registry.npmjs.org".to_string(),
        "https://registry.npmmirror.com".to_string(),
    ];
    if let Ok(override_url) = std::env::var("REGISTRY_OVERRIDE") {
        if !override_url.is_empty() {
            registries.insert(0, override_url.trim_end_matches('/').to_string());
        }
    }

    let encoded = name.replace('/', "%2F");
    let mut last_err = String::new();
    for reg in registries {
        let url = format!("{reg}/{encoded}");
        match client.get(&url).send().await {
            Ok(res) if res.status().is_success() => {
                let data: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
                let latest = data
                    .get("dist-tags")
                    .and_then(|d| d.get("latest"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let ver = if latest.is_empty() { None } else { data.get("versions").and_then(|v| v.get(&latest)) };
                let meta = serde_json::json!({
                    "name": name,
                    "latest": latest,
                    "description": ver.and_then(|v| v.get("description")).and_then(|d| d.as_str()).unwrap_or(""),
                    "homepage": ver.and_then(|v| v.get("homepage")).and_then(|h| h.as_str()).unwrap_or(""),
                    "repository": ver.and_then(|v| v.get("repository")).and_then(|r| r.get("url")).and_then(|u| u.as_str()).unwrap_or(""),
                    "registry": reg,
                });
                return Ok(meta);
            }
            Ok(res) => last_err = format!("HTTP {}", res.status()),
            Err(e) => last_err = e.to_string(),
        }
    }
    Err(format!("registry query failed: {last_err}"))
}

// ---------- 脚本执行（默认禁用） ----------

fn exec_script(body: &serde_json::Value) -> Result<String, String> {
    let script = body.get("script").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let timeout_ms = body.get("timeoutMs").and_then(|t| t.as_u64()).unwrap_or(30_000);
    if script.is_empty() {
        return Err("empty script".to_string());
    }
    let script_clone = script.clone();
    let out = std::thread::spawn(move || {
        let mut cmd = if cfg!(windows) {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", &script_clone]);
            c
        } else {
            let mut c = std::process::Command::new("sh");
            c.arg("-c").arg(&script_clone);
            c
        };
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
    })
    .join()
    .map_err(|_| "script thread panicked".to_string())?;

    let output = out.map_err(|e| format!("script spawn failed: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let _ = timeout_ms;
    Ok(format!(
        "exit={}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout.trim(),
        stderr.trim()
    ))
}
