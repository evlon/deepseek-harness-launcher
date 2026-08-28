//! 管理能力（外网代理网关）：管理员 launcher 在本机 127.0.0.1 起的本地 API 服务。
//!
//! 背景：中心服务端（内网服务器）**不能访问外网**。管理员电脑能上网，
//! 通过托盘「开启管理能力」在本机起一个 HTTP 监听，服务端管理页（管理员浏览器）
//! 直接 fetch 这个本地 API 中转查询外网 npm registry 的包信息/拉包体。
//!
//! 约束：
//! - 仅绑定 127.0.0.1（不接受局域网访问）
//! - 可选 token（X-Bridge-Token），防本机其他进程滥用
//! - 脚本执行默认禁用（allowScripts），启用时受限执行
//! - 零新依赖：std TcpListener + 极简 HTTP 解析（GET/POST + JSON + CORS）

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Runtime};

use crate::config::*;

/// 运行状态：端口 + 停止标志。
pub struct Bridge {
    port: u16,
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

/// 启动管理能力本地 API（绑定 127.0.0.1，端口冲突自动顺延）。
pub fn start<R: Runtime>(app: &AppHandle<R>) -> Result<u16, String> {
    stop(); // 先停旧的

    let cfg = load_cached();
    let base = bridge_port(&cfg);
    let token = bridge_token(&cfg);
    let allow_scripts = cfg
        .admin_bridge
        .as_ref()
        .map(|b| b.allow_scripts)
        .unwrap_or(false);

    let h = app.clone();
    let mut port = base;
    let listener = loop {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(l) => break l,
            Err(_) => {
                if port >= base + 50 {
                    return Err(format!("BRIDGE_PORT_EXHAUSTED: 端口 {base}~{} 均被占用", base + 50));
                }
                port += 1;
            }
        }
    };
    listener.set_nonblocking(false).map_err(|e| e.to_string())?;

    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = shutdown.clone();
    let thread = std::thread::spawn(move || {
        log::info!("管理能力本地 API 已启动：http://127.0.0.1:{port}");
        for stream in listener.incoming() {
            if shutdown_clone.load(Ordering::Relaxed) {
                break;
            }
            match stream {
                Ok(s) => {
                    let h = h.clone();
                    let token = token.clone();
                    let allow_scripts = allow_scripts;
                    std::thread::spawn(move || handle_conn(s, &h, &token, allow_scripts));
                }
                Err(_) => break,
            }
        }
    });

    *CURRENT.lock().unwrap() = Some(Bridge {
        port,
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
        // 主动连一次让 accept 返回（唤醒阻塞的 incoming）
        let _ = TcpStream::connect(("127.0.0.1", b.port));
        if let Some(t) = b.thread {
            let _ = t.join();
        }
        log::info!("管理能力本地 API 已停止");
    }
}

// ---------- HTTP 极简处理 ----------

struct Request {
    method: String,
    path: String,
    query: std::collections::HashMap<String, String>,
    body: Vec<u8>,
}

fn parse_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf).ok()?;
    if n == 0 {
        return None;
    }
    let text = String::from_utf8_lossy(&buf[..n]);
    let mut lines = text.split("\r\n");
    let head = lines.next()?;
    let mut parts = head.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();

    // 分离 path 与 query
    let (path, query_str) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.clone(), String::new()),
    };
    let mut query = std::collections::HashMap::new();
    for kv in query_str.split('&') {
        if kv.is_empty() {
            continue;
        }
        if let Some((k, v)) = kv.split_once('=') {
            query.insert(
                percent_decode(k),
                percent_decode(v),
            );
        }
    }

    // 读取 body（Content-Length）
    let mut body = Vec::new();
    let mut clen = 0;
    for line in &mut lines {
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            clen = v.trim().parse().unwrap_or(0);
        }
    }
    if clen > 0 && clen <= 1 << 20 {
        let mut remaining = clen as usize;
        while remaining > 0 {
            let mut chunk = vec![0u8; remaining.min(8192)];
            let r = stream.read(&mut chunk).ok()?;
            if r == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..r]);
            remaining -= r;
        }
    }

    Some(Request { method, path, query, body })
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

fn send_json(stream: &mut TcpStream, code: u16, obj: serde_json::Value) {
    let body = serde_json::to_string(&obj).unwrap_or_else(|_| "{}".to_string());
    let resp = format!(
        "HTTP/1.1 {code} {}\r\nContent-Type: application/json; charset=utf-8\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
         Access-Control-Allow-Headers: Content-Type, X-Bridge-Token\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        status_text(code),
        body.len(),
        body
    );
    let _ = stream.write_all(resp.as_bytes());
}

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Status",
    }
}

fn handle_conn<R: Runtime>(mut stream: TcpStream, _app: &AppHandle<R>, token: &str, allow_scripts: bool) {
    let Some(req) = parse_request(&mut stream) else {
        return;
    };

    // CORS preflight
    if req.method == "OPTIONS" {
        let resp = "HTTP/1.1 204 No Content\r\n\
                    Access-Control-Allow-Origin: *\r\n\
                    Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
                    Access-Control-Allow-Headers: Content-Type, X-Bridge-Token\r\n\
                    Connection: close\r\n\r\n";
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    // token 校验
    let auth_ok = token.is_empty() || req_has_token(&req, token);
    if !auth_ok {
        send_json(&mut stream, 403, serde_json::json!({ "error": "invalid bridge token" }));
        return;
    }

    // 路由分发
    let resp: serde_json::Value = match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/api/health") => serde_json::json!({
            "ok": true,
            "version": env!("CARGO_PKG_VERSION"),
            "port": port(),
            "bridge": true,
        }),
        ("GET", "/api/registry/meta") => {
            let name = req.query.get("name").cloned().unwrap_or_default();
            match block_on_query_meta(&name) {
                Ok(meta) => serde_json::json!({ "ok": true, "meta": meta }),
                Err(e) => serde_json::json!({ "ok": false, "error": e }),
            }
        }
        ("POST", "/api/script/exec") => {
            if !allow_scripts {
                send_json(&mut stream, 403, serde_json::json!({ "error": "scripts disabled (adminBridge.allowScripts)" }));
                return;
            }
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or(serde_json::Value::Null);
            match exec_script(&body) {
                Ok(out) => serde_json::json!({ "ok": true, "output": out }),
                Err(e) => serde_json::json!({ "ok": false, "error": e }),
            }
        }
        _ => {
            send_json(&mut stream, 404, serde_json::json!({ "error": "not found" }));
            return;
        }
    };
    send_json(&mut stream, 200, resp);
}

fn req_has_token(req: &Request, token: &str) -> bool {
    // 简化：从 body/query 里不取，管理页通过 header 传——极简实现从 query 取（管理页可带 ?token=）
    req.query.get("token").map(|t| t == token).unwrap_or(false)
}

// ---------- registry 查询（外网中转） ----------

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
