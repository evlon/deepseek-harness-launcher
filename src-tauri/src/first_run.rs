//! 首次使用引导窗口：解决「双击后没反应」——托盘图标被 Windows 折叠进 `^`，
//! 小白看不到任何反馈。首次运行主动弹引导窗口，作为「激活数字分身」的主流程首屏。
//!
//! 与 matrix_setup 同机制：Tauri 自定义协议窗口 + 内嵌 HTML（无需前端构建）。
//!
//! ⚠️ 关键盲区（2026-09-23 实测修复）：早期版本 `is_first_run` 只判「dsh 核心是否
//! 安装」，而「dsh 已装但数字分身未激活」这一态（matrix profile 未建 / 未激活）
//! 既不弹欢迎窗、也不弹激活向导，程序静默缩进托盘——同事双击后「啥也不知道」。
//! 因此引入统一判定 `needs_onboarding`：只要「dsh 未装」**或**「数字分身未激活」，
//! 都属于需要引导的首次态，双击后直接进入「激活数字分身」主流程。

use tauri::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};

use crate::config::*;

/// 窗口 label。
const WINDOW_LABEL: &str = "first-run";

/// 是否首次运行（dsh 核心未安装 = 全新机器/未完成安装）。
/// 仅用于「纯安装」判断；引导逻辑请统一用 [`needs_onboarding`]。
pub fn is_first_run<R: Runtime>(app: &AppHandle<R>) -> bool {
    !dsh_binary_path(app).exists()
}

/// 是否处于「首次引导态」：dsh 核心未安装，**或**数字分身未激活。
///
/// 这是决定「双击后是否直接进入激活主流程」的统一判定，覆盖早期版本的两个盲区：
/// - ① dsh 未装 → 需要引导（先装依赖再激活）；
/// - ② dsh 已装但 matrix profile 未装 / 分身未配置 → 同样需要引导（直接激活）。
///
/// 只有「数字分身已激活（Configured）」才返回 false（此时用户已完成首次使用，
/// 双击保持托盘常驻行为，不骚扰）。
pub fn needs_onboarding<R: Runtime>(app: &AppHandle<R>) -> bool {
    if is_first_run(app) {
        return true;
    }
    let cfg = load_cached();
    !matches!(
        crate::matrix_setup::status(app, &cfg),
        crate::matrix_setup::MatrixStatus::Configured
    )
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

/// first-run scheme 协议处理：
///   GET  /          → 激活主流程首屏 HTML
///   GET  /state     → 当前引导态（{ dshInstalled, activated, pendingPlugins }）
///   POST /activate  → 开始「装依赖 + 激活数字分身」一条龙
///   POST /close     → 关窗（用户主动「稍后再说」，缩回托盘）
pub fn handle_scheme_request<R: Runtime>(
    ctx: &tauri::UriSchemeContext<'_, R>,
    request: tauri::http::Request<Vec<u8>>,
) -> tauri::http::Response<Vec<u8>> {
    use tauri::http::{header, Response, StatusCode};
    let app = ctx.app_handle();
    let path = request.uri().path().to_string();
    let method = request.method().clone();
    log::info!("first-run:// 协议请求：{method} {path}");

    let json_resp = |status: StatusCode, obj: serde_json::Value| -> Response<Vec<u8>> {
        Response::builder()
            .status(status)
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
    if method == tauri::http::Method::GET && path == "/state" {
        let cfg = load_cached();
        let dsh_installed = dsh_binary_path(app).exists();
        let st = crate::matrix_setup::status(app, &cfg);
        let activated = matches!(st, crate::matrix_setup::MatrixStatus::Configured);
        return json_resp(
            StatusCode::OK,
            serde_json::json!({
                "dshInstalled": dsh_installed,
                "activated": activated,
            }),
        );
    }
    if method == tauri::http::Method::POST && path == "/activate" {
        // 「激活数字分身」主流程：dsh 未装 → 先 install_all（含进度窗），
        // 装完自动衔接激活向导；dsh 已装 → 直接打开激活向导。
        // 激活向导（matrix-setup）内已有「自动激活 + 选岗位」完整步骤。
        let h = app.clone();
        let dsh_installed = dsh_binary_path(app).exists();
        if !dsh_installed {
            // 后台跑安装（复用 install_all：含进度窗口 + 分步通知）。
            // 装完关闭本首屏，衔接数字分身激活向导（而非旧的「配置向导」）。
            tauri::async_runtime::spawn(async move {
                let _ = crate::install::install_all(&h).await;
                close_window(&h);
                log::info!("首次安装完成，自动打开数字分身激活向导");
                let _ = crate::matrix_setup::open_window(&h);
            });
            return json_resp(
                StatusCode::OK,
                serde_json::json!({"ok": true, "message": "正在安装依赖，随后自动进入激活"}),
            );
        }
        // dsh 已装 → 直接打开激活向导
        tauri::async_runtime::spawn(async move {
            let _ = crate::matrix_setup::open_window(&h);
        });
        return json_resp(
            StatusCode::OK,
            serde_json::json!({"ok": true, "message": "已打开激活向导"}),
        );
    }
    if method == tauri::http::Method::POST && path == "/close" {
        close_window(app);
        return json_resp(StatusCode::OK, serde_json::json!({"ok": true}));
    }
    json_resp(StatusCode::NOT_FOUND, serde_json::json!({"ok": false, "error": "not found"}))
}

/// 激活主流程首屏 HTML（小白向：双击后第一眼就告诉他「点这里激活数字分身」）。
///
/// 文案目标：让用户**一眼看懂下一步**，不再需要「找托盘、问同事」。
/// 主按钮 = 「开始激活」，后端按状态自动分流（未装 dsh 先装、已装直接激活）。
fn welcome_html() -> String {
    r#"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<title>激活数字分身</title>
<style>
  :root{--bg:#1a2233;--card:#232c40;--text:#e6eaf2;--muted:#8b95a9;--green:#4ade80;--blue:#60a5fa;--line:#2d3650}
  *{box-sizing:border-box;margin:0;padding:0}
  body{font-family:-apple-system,"Segoe UI",Roboto,"PingFang SC","Microsoft YaHei",sans-serif;background:var(--bg);color:var(--text);padding:24px;font-size:13.5px;line-height:1.7}
  h1{font-size:20px;margin-bottom:6px}
  .sub{font-size:12.5px;color:var(--muted);margin-bottom:18px}
  .card{background:var(--card);border:1px solid var(--line);border-radius:10px;padding:15px;margin-bottom:12px}
  .card h2{font-size:13px;color:var(--blue);margin-bottom:8px}
  .steps{list-style:none;counter-reset:s}
  .steps li{counter-increment:s;position:relative;padding-left:26px;margin:7px 0;font-size:13px}
  .steps li::before{content:counter(s);position:absolute;left:0;top:1px;width:18px;height:18px;border-radius:50%;
    background:var(--blue);color:#fff;font-size:11px;font-weight:700;display:flex;align-items:center;justify-content:center}
  .tip{background:#1f2937;border-left:3px solid var(--blue);border-radius:6px;padding:10px 12px;font-size:12.5px;color:var(--muted);margin-bottom:16px}
  .tip b{color:var(--text)}
  .btn{width:100%;padding:13px;border-radius:9px;border:0;cursor:pointer;font-size:15px;font-weight:700;margin-bottom:9px}
  .btn-primary{background:var(--green);color:#fff}
  .btn-primary:disabled{opacity:.55;cursor:not-allowed}
  .btn-ghost{background:transparent;color:var(--muted);border:1px solid var(--line);font-weight:500;font-size:13px}
  .btn-ghost:hover{color:var(--text)}
  .status{font-size:12.5px;min-height:18px;text-align:center;color:var(--muted)}
  .status.ok{color:var(--green)} .status.err{color:#f87171}
</style>
</head>
<body>
  <h1>🤖 激活你的数字分身</h1>
  <div class="sub">欢迎使用！点下面的按钮，用公司账号一键认领你的数字分身。</div>

  <div class="card">
    <h2>接下来会自动完成</h2>
    <ol class="steps">
      <li>下载并安装运行环境（首次约几分钟）</li>
      <li>用公司账号登录，自动认领你的 @ai-xxx 数字分身</li>
      <li>选择你的岗位，分身即可在聊天工具里 @ 使用</li>
    </ol>
  </div>

  <div class="tip">
    <b>小提示：</b>完成后数字分身常驻在右下角托盘区（点 <b>^</b> 可看到图标），
    以后从托盘图标打开它。
  </div>

  <button class="btn btn-primary" id="activate">🚀 开始激活</button>
  <button class="btn btn-ghost" id="close">稍后再说（可从托盘图标随时打开）</button>
  <div class="status" id="status"></div>

<script>
(function(){
  const $=id=>document.getElementById(id);
  $("activate").onclick=async()=>{
    const st=$("status"); st.className="status"; st.textContent="正在准备…";
    $("activate").disabled=true;
    try{
      const r=await fetch("http://first-run.localhost/activate",{method:"POST"});
      const j=await r.json();
      if(j.ok){
        st.className="status ok";
        st.textContent="✓ 已开始——接下来会自动安装环境并进入激活，请稍候。";
      } else { st.className="status err"; st.textContent="✗ "+(j.error||"启动失败"); $("activate").disabled=false; }
    }catch(e){ st.className="status err"; st.textContent="✗ 无法连接本机服务"; $("activate").disabled=false; }
  };
  $("close").onclick=()=>{ fetch("http://first-run.localhost/close",{method:"POST"}).catch(()=>{}); };
})();
</script>
</body>
</html>"#
        .to_string()
}
