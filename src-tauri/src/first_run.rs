//! 首次运行引导窗口：解决「双击后没反应」——托盘图标被 Windows 折叠进 `^`，
//! 小白看不到任何反馈。首次运行（dsh 未安装）主动弹欢迎窗口：
//! 说明程序已在托盘运行 + 一键安装入口，装完自动衔接数字分身配置向导。
//!
//! 与 matrix_setup 同机制：Tauri 自定义协议窗口 + 内嵌 HTML（无需前端构建）。

use tauri::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};

use crate::config::*;

/// 窗口 label。
const WINDOW_LABEL: &str = "first-run";

/// 是否首次运行（dsh 核心未安装 = 全新机器/未完成安装）。
pub fn is_first_run<R: Runtime>(app: &AppHandle<R>) -> bool {
    !dsh_binary_path(app).exists()
}

/// 打开（或聚焦）首次运行欢迎窗口。
pub fn open_window<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    if let Some(win) = app.get_webview_window(WINDOW_LABEL) {
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(());
    }
    let url = WebviewUrl::External(
        "http://first-run.localhost/index.html"
            .parse()
            .map_err(|e: url::ParseError| e.to_string())?,
    );
    WebviewWindowBuilder::new(app, WINDOW_LABEL, url)
        .title("数字分身 · 首次使用")
        .inner_size(520.0, 480.0)
        .resizable(true)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 关闭欢迎窗口（安装开始后调用）。
pub fn close_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(win) = app.get_webview_window(WINDOW_LABEL) {
        let _ = win.close();
    }
}

/// first-run scheme 协议处理：GET / → HTML；POST /install → 一键安装；POST /close → 关窗。
pub fn handle_scheme_request<R: Runtime>(
    ctx: &tauri::UriSchemeContext<'_, R>,
    request: tauri::http::Request<Vec<u8>>,
) -> tauri::http::Response<Vec<u8>> {
    use tauri::http::{header, Response, StatusCode};
    let app = ctx.app_handle();
    let path = request.uri().path().to_string();
    let method = request.method().clone();
    log::info!("first-run:// 协议请求：{method} {path}");

    let json_resp = |obj: serde_json::Value| -> Response<Vec<u8>> {
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
            .body(serde_json::to_string(&obj).unwrap_or_else(|_| "{}".into()).into_bytes())
            .unwrap_or_default()
    };

    if method == tauri::http::Method::GET && (path == "/" || path == "/index.html") {
        let html = welcome_html();
        return Response::builder()
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(
                "Content-Security-Policy",
                "default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self' http://first-run.localhost",
            )
            .body(html.into_bytes())
            .unwrap_or_default();
    }
    if method == tauri::http::Method::POST && path == "/install" {
        // 后台跑安装（复用 install_all：含进度窗口 + 分步通知）
        let h = app.clone();
        tauri::async_runtime::spawn(async move {
            let _ = crate::install::install_all(&h).await;
            // 装完关闭欢迎窗口 + 衔接数字分身配置向导（若仍未配置）
            close_window(&h);
            let cfg = load_cached();
            if crate::matrix_setup::matrix_agent_installed(&h, &cfg)
                && matches!(
                    crate::matrix_setup::status(&h, &cfg),
                    crate::matrix_setup::MatrixStatus::Unconfigured { .. }
                )
            {
                log::info!("首次安装完成，自动打开数字分身配置向导");
                let _ = crate::matrix_setup::open_window(&h);
            }
        });
        return json_resp(serde_json::json!({"ok": true, "message": "安装已开始"}));
    }
    if method == tauri::http::Method::POST && path == "/close" {
        close_window(app);
        return json_resp(serde_json::json!({"ok": true}));
    }
    json_resp(serde_json::json!({"ok": false, "error": "not found"}))
}

/// 欢迎窗口 HTML（小白向：先说明「我已在运行」，再给一键安装）。
fn welcome_html() -> String {
    r#"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<title>数字分身 · 首次使用</title>
<style>
  :root{--bg:#1a2233;--card:#232c40;--text:#e6eaf2;--muted:#8b95a9;--green:#4ade80;--blue:#60a5fa;--line:#2d3650}
  *{box-sizing:border-box;margin:0;padding:0}
  body{font-family:-apple-system,"Segoe UI",Roboto,"PingFang SC","Microsoft YaHei",sans-serif;background:var(--bg);color:var(--text);padding:22px;font-size:13.5px;line-height:1.7}
  h1{font-size:19px;margin-bottom:6px}
  .sub{font-size:12.5px;color:var(--muted);margin-bottom:16px}
  .card{background:var(--card);border:1px solid var(--line);border-radius:10px;padding:15px;margin-bottom:12px}
  .card h2{font-size:13px;color:var(--blue);margin-bottom:8px}
  .steps{list-style:none;counter-reset:s}
  .steps li{counter-increment:s;position:relative;padding-left:26px;margin:7px 0;font-size:13px}
  .steps li::before{content:counter(s);position:absolute;left:0;top:1px;width:18px;height:18px;border-radius:50%;
    background:var(--blue);color:#fff;font-size:11px;font-weight:700;display:flex;align-items:center;justify-content:center}
  .tip{background:#1f2937;border-left:3px solid var(--blue);border-radius:6px;padding:10px 12px;font-size:12.5px;color:var(--muted);margin-bottom:16px}
  .tip b{color:var(--text)}
  .btn{width:100%;padding:12px;border-radius:9px;border:0;cursor:pointer;font-size:14.5px;font-weight:700;margin-bottom:9px}
  .btn-primary{background:var(--blue);color:#fff}
  .btn-primary:disabled{opacity:.55;cursor:not-allowed}
  .btn-ghost{background:transparent;color:var(--muted);border:1px solid var(--line);font-weight:500;font-size:13px}
  .btn-ghost:hover{color:var(--text)}
  .status{font-size:12.5px;min-height:18px;text-align:center;color:var(--muted)}
  .status.ok{color:var(--green)} .status.err{color:#f87171}
</style>
</head>
<body>
  <h1>👋 欢迎使用数字分身</h1>
  <div class="sub">程序已经启动了——它常驻在右下角托盘区，不会弹出主界面。</div>

  <div class="tip">
    <b>找不到它？</b>看屏幕右下角任务栏，点一下 <b>^</b> 小箭头展开隐藏图标，就能看到本程序的图标；
    建议右键图标选「固定到任务栏」方便以后使用。
  </div>

  <div class="card">
    <h2>接下来只需 3 步</h2>
    <ol class="steps">
      <li>点下面的「一键安装」，等待依赖下载完成（约几分钟）</li>
      <li>安装完成后会自动弹出配置窗口，填入数字分身账号</li>
      <li>配置完成即可在聊天工具里 @ 你的数字分身</li>
    </ol>
  </div>

  <button class="btn btn-primary" id="install">🚀 一键安装（首次必点）</button>
  <button class="btn btn-ghost" id="close">稍后再说（可右键托盘图标随时安装）</button>
  <div class="status" id="status"></div>

<script>
(function(){
  const $=id=>document.getElementById(id);
  $("install").onclick=async()=>{
    const st=$("status"); st.className="status"; st.textContent="正在开始安装…";
    $("install").disabled=true;
    try{
      const r=await fetch("http://first-run.localhost/install",{method:"POST"});
      const j=await r.json();
      if(j.ok){
        st.className="status ok";
        st.textContent="✓ 已开始安装——进度窗口即将弹出，本窗口会自动关闭。";
      } else { st.className="status err"; st.textContent="✗ "+(j.error||"启动安装失败"); $("install").disabled=false; }
    }catch(e){ st.className="status err"; st.textContent="✗ 无法连接本机服务"; $("install").disabled=false; }
  };
  $("close").onclick=()=>{ fetch("http://first-run.localhost/close",{method:"POST"}).catch(()=>{}); };
})();
</script>
</body>
</html>"#
        .to_string()
}
