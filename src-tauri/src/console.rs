//! 操作窗口：Tauri 动态创建的无边框小窗口，内嵌 HTML 实时展示操作进度/日志。
//!
//! 无窗口应用（windows: []）运行时通过 `WebviewWindowBuilder` 动态创建，
//! 用 `data:` URL 内嵌 HTML（无需前端构建）。前端 `listen("op-update")`
//! 接收 ops.rs 推送的更新，渲染步骤列表 / 进度 / 日志。

use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};

/// 打开（或聚焦）操作窗口。失败返回错误信息（调用方降级为仅托盘状态）。
pub fn open_console<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    // 已存在则聚焦
    if let Some(win) = app.get_webview_window("op-console") {
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(());
    }

    // 用自定义协议加载内嵌 HTML（data: URL 被 Tauri 2 External 安全策略拦截）
    let url = WebviewUrl::External("console://localhost/index.html".parse().map_err(|e: url::ParseError| e.to_string())?);

    let window = WebviewWindowBuilder::new(app, "op-console", url)
        .title("操作进度")
        .inner_size(440.0, 600.0)
        .resizable(true)
        .build()
        .map_err(|e| e.to_string())?;

    // 窗口关闭时清理（不推送状态）
    let _ = window.on_window_event(move |event| {
        let _ = event;
    });

    // 推送当前状态（若窗口刚建好时已有操作）
    if let Some(op) = crate::ops::current() {
        let _ = app.emit("op-update", op);
    }
    Ok(())
}

/// 操作窗口的内嵌 HTML（由 main.rs 注册的 `console://` 协议返回）。
pub fn console_html() -> String {
    r#"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<title>操作进度</title>
<style>
  :root{--bg:#1a2233;--card:#232c40;--text:#e6eaf2;--muted:#8b95a9;--green:#4ade80;--amber:#fbbf24;--red:#f87171;--blue:#60a5fa}
  *{box-sizing:border-box;margin:0;padding:0}
  body{font-family:-apple-system,"Segoe UI",Roboto,"PingFang SC","Microsoft YaHei",sans-serif;background:var(--bg);color:var(--text);padding:16px;font-size:13px;height:100vh;overflow:hidden;display:flex;flex-direction:column}
  .title{font-size:15px;font-weight:700;margin-bottom:4px}
  .subtitle{font-size:12px;color:var(--muted);margin-bottom:12px}
  .progress-wrap{background:var(--card);border-radius:8px;padding:12px;margin-bottom:12px}
  .progress-label{font-size:12.5px;margin-bottom:6px;display:flex;justify-content:space-between}
  .bar{height:8px;background:#2d3650;border-radius:4px;overflow:hidden}
  .bar-fill{height:100%;background:linear-gradient(90deg,var(--blue),var(--green));border-radius:4px;width:0%;transition:width .3s}
  .steps{background:var(--card);border-radius:8px;padding:12px;margin-bottom:12px;max-height:180px;overflow-y:auto}
  .step{display:flex;align-items:center;gap:8px;padding:3px 0;font-size:12.5px}
  .step .mark{width:18px;text-align:center}
  .step.pending{color:var(--muted)} .step.running{color:var(--text)} .step.done{color:var(--green)} .step.failed{color:var(--red)}
  .log{flex:1;background:var(--card);border-radius:8px;padding:10px 12px;overflow-y:auto;font-family:ui-monospace,Consolas,monospace;font-size:11.5px;line-height:1.7;min-height:100px}
  .log .info{color:var(--muted)} .log .err{color:var(--red)}
  .status{font-size:12px;margin-top:8px;color:var(--muted);text-align:center}
</style>
</head>
<body>
  <div class="title">DeepSeek Harness Launcher</div>
  <div class="subtitle" id="opLabel">就绪</div>
  <div class="progress-wrap">
    <div class="progress-label"><span id="opStep">—</span><span id="opPercent"></span></div>
    <div class="bar"><div class="bar-fill" id="opBar"></div></div>
  </div>
  <div class="steps" id="opSteps"></div>
  <div class="log" id="opLog"></div>
  <div class="status" id="opStatus"></div>

<script>
(function(){
  const labelEl=document.getElementById("opLabel");
  const stepEl=document.getElementById("opStep");
  const pctEl=document.getElementById("opPercent");
  const barEl=document.getElementById("opBar");
  const stepsEl=document.getElementById("opSteps");
  const logEl=document.getElementById("opLog");
  const statusEl=document.getElementById("opStatus");

  function esc(s){ return String(s).replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;"); }

  function render(op){
    if(!op){ labelEl.textContent="就绪"; stepEl.textContent="—"; pctEl.textContent=""; barEl.style.width="0%"; stepsEl.innerHTML=""; logEl.innerHTML=""; statusEl.textContent="无进行中的操作"; return; }
    labelEl.textContent=op.label||"操作";
    stepEl.textContent=op.current_step||"…";
    // 进度：从 current_step 里提取百分比（如 "下载 45%"）或按步骤算
    let pct=0;
    if(op.state==="done") pct=100;
    else if(op.state==="failed") pct=100;
    else if(op.steps && op.steps.length){
      let done=op.steps.filter(s=>s.state==="done"||s.state==="failed").length;
      pct=Math.round(done/op.steps.length*100);
    }
    barEl.style.width=pct+"%";
    pctEl.textContent=pct? (pct+"%") : "";

    // 步骤列表
    if(op.steps && op.steps.length){
      stepsEl.innerHTML=op.steps.map(s=>{
        const cls=s.state==="done"?"done":(s.state==="running"?"running":(s.state==="failed"?"failed":"pending"));
        const mark=s.state==="done"?"✓":(s.state==="running"?"⏳":(s.state==="failed"?"✗":"○"));
        return '<div class="step '+cls+'"><span class="mark">'+mark+'</span>'+esc(s.label)+'</div>';
      }).join("");
    }

    // 日志
    if(op.log && op.log.length){
      logEl.innerHTML=op.log.map(l=>{
        const isErr=l.startsWith("[失败]")||l.startsWith("[错误]");
        return '<div class="'+(isErr?"err":"info")+'">'+esc(l)+'</div>';
      }).join("");
      logEl.scrollTop=logEl.scrollHeight;
    }

    // 状态
    statusEl.textContent = op.state==="done" ? ("✓ 完成："+(op.result||""))
      : op.state==="failed" ? ("✗ 失败："+(op.result||""))
      : "进行中…";
    statusEl.style.color = op.state==="failed" ? "var(--red)" : (op.state==="done" ? "var(--green)" : "var(--muted)");
  }

  // 监听更新
  window.__TAURI__ && window.__TAURI__.event.listen("op-update", (e)=>{ render(e.payload); });
})();
</script>
</body>
</html>"#
        .to_string()
}
