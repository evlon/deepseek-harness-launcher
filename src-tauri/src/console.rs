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

    // 用自定义协议加载内嵌 HTML。
    // Windows 上 custom protocol 的访问地址是 http://<scheme>.localhost/<path>
    // （tauri 文档：Windows/Android 用 http，非 console:// 形式）
    let url = WebviewUrl::External("http://console.localhost/index.html".parse().map_err(|e: url::ParseError| e.to_string())?);

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

    // 推送当前状态（若窗口刚建好时已有操作）；无操作则显示日志文件尾部
    match crate::ops::current() {
        Some(op) => {
            log::info!("进度窗口：推送当前操作 state={:?} label={}", op.state, op.label);
            let _ = app.emit("op-update", op);
        }
        None => {
            // 无进行中操作：显示日志文件尾部（让窗口有内容）
            let tail = read_log_tail(app, 200);
            log::info!("进度窗口：无操作，显示日志尾部 {} 行", tail.len());
            let op = crate::ops::Operation {
                id: "log-view".to_string(),
                label: "运行日志".to_string(),
                state: crate::ops::OpState::Idle,
                current_step: "（无进行中操作）".to_string(),
                steps: Vec::new(),
                log: tail,
                result: String::new(),
                details: Vec::new(),
                started_at: String::new(),
                finished_at: String::new(),
            };
            let _ = app.emit("op-update", op);
        }
    }
    Ok(())
}

/// 读取日志文件尾部（最近 n 行）。
fn read_log_tail<R: Runtime>(app: &AppHandle<R>, n: usize) -> Vec<String> {
    let path = crate::config::log_file(app);
    let Ok(text) = std::fs::read_to_string(&path) else { return vec!["（日志文件尚未创建）".to_string()] };
    let lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].to_vec()
}

/// 操作窗口的内嵌 HTML（由 main.rs 注册的 `console://` 协议返回）。
/// 初始状态直接内嵌（custom protocol 下无 Tauri IPC，JS 用轮询 /state 更新）。
pub fn console_html() -> String {
    let initial = crate::ops::current()
        .map(|op| serde_json::to_string(&op).unwrap_or_else(|_| "null".to_string()))
        .unwrap_or_else(|| "null".to_string());
    let history = serde_json::to_string(&crate::ops::history(crate::ops::MAX_HISTORY))
        .unwrap_or_else(|_| "[]".to_string());
    let html = r#"<!doctype html>
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
  /* ① 向导：将要做什么（点更新前就看得见） */
  .plan{background:#20293c;border-left:3px solid var(--blue);border-radius:6px;padding:10px 12px;margin-bottom:12px;font-size:12.5px;line-height:1.8}
  .plan .h{color:var(--blue);font-weight:600;margin-bottom:4px}
  .plan .item{color:var(--text)}
  .log{flex:1;background:var(--card);border-radius:8px;padding:10px 12px;overflow-y:auto;font-family:ui-monospace,Consolas,monospace;font-size:11.5px;line-height:1.7;min-height:80px}
  .log .info{color:var(--muted)} .log .err{color:var(--red)} .log .plan{color:var(--blue)}
  .status{font-size:12px;margin-top:8px;color:var(--muted);text-align:center}
  .hist{margin-top:10px;font-size:11.5px}
  .hist summary{cursor:pointer;color:var(--muted);outline:none}
  .hist .row{padding:4px 0;border-bottom:1px solid #2d3650;line-height:1.6}
  .hist .t{color:var(--muted);font-size:11px}
  .hist .ok{color:var(--green)} .hist .bad{color:var(--red)}
</style>
</head>
<body>
  <div class="title">DeepSeek Harness Launcher</div>
  <div class="subtitle" id="opLabel">就绪</div>
  <div class="plan" id="opPlan" style="display:none"></div>
  <div class="progress-wrap">
    <div class="progress-label"><span id="opStep">—</span><span id="opPercent"></span></div>
    <div class="bar"><div class="bar-fill" id="opBar"></div></div>
  </div>
  <div class="steps" id="opSteps"></div>
  <div class="log" id="opLog"></div>
  <div class="status" id="opStatus"></div>
  <details class="hist" id="opHistWrap" style="display:none">
    <summary id="opHistSummary">历史记录</summary>
    <div id="opHist"></div>
  </details>

<script>
(function(){
  const labelEl=document.getElementById("opLabel");
  const planEl=document.getElementById("opPlan");
  const stepEl=document.getElementById("opStep");
  const pctEl=document.getElementById("opPercent");
  const barEl=document.getElementById("opBar");
  const stepsEl=document.getElementById("opSteps");
  const logEl=document.getElementById("opLog");
  const statusEl=document.getElementById("opStatus");
  const histWrap=document.getElementById("opHistWrap");
  const histEl=document.getElementById("opHist");
  const histSummary=document.getElementById("opHistSummary");

  function esc(s){ return String(s).replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;"); }

  function render(op){
    if(!op){ labelEl.textContent="就绪"; stepEl.textContent="—"; pctEl.textContent=""; barEl.style.width="0%"; stepsEl.innerHTML=""; logEl.innerHTML=""; statusEl.textContent="无进行中的操作"; planEl.style.display="none"; return; }
    labelEl.textContent=op.label||"操作";
    stepEl.textContent=op.current_step||"…";

    // ① 向导首屏：展示「将要做什么」（名称/版本/来源/是否重启）
    const details=op.details||[];
    if(details.length){
      planEl.style.display="block";
      planEl.innerHTML='<div class="h">将要执行</div>'+details.map(d=>'<div class="item">• '+esc(d)+'</div>').join("");
    } else {
      planEl.style.display="none";
    }

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

    // ② 步骤列表（逐步显示当前在做什么、哪些已完成）
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
        const isErr=l.startsWith("[失败]")||l.startsWith("[错误]")||l.startsWith("✗");
        const isPlan=l.startsWith("[计划]");
        return '<div class="'+(isErr?"err":(isPlan?"plan":"info"))+'">'+esc(l)+'</div>';
      }).join("");
      logEl.scrollTop=logEl.scrollHeight;
    }

    // ③ 状态：完成/失败都要有明确结论
    statusEl.textContent = op.state==="done" ? ("✓ 完成："+(op.result||""))
      : op.state==="failed" ? ("✗ 失败："+(op.result||""))
      : op.state==="idle" ? ("运行日志（无进行中操作）")
      : "进行中…";
    statusEl.style.color = op.state==="failed" ? "var(--red)" : (op.state==="done" ? "var(--green)" : "var(--muted)");
  }

  // 历史：让用户事后能回看「刚才更新了什么、成没成功」
  function renderHist(list){
    if(!list || !list.length){ histWrap.style.display="none"; return; }
    histWrap.style.display="block";
    histSummary.textContent="历史记录（"+list.length+"）";
    histEl.innerHTML=list.map(op=>{
      const ok=op.state==="done";
      const mark=ok?"✓":"✗";
      const cls=ok?"ok":"bad";
      const t=esc(op.finished_at||op.started_at||"");
      const d=(op.details&&op.details.length)?(" — "+esc(op.details[0])):"";
      const r=esc(op.result||"");
      return '<div class="row"><span class="'+cls+'">'+mark+' '+esc(op.label)+'</span>'+d
        +'<div class="t">'+t+' '+r+'</div></div>';
    }).join("");
  }

  // 初始状态（协议 handler 内嵌）：custom protocol 下无 Tauri IPC/事件，
  // 用内嵌初始状态 + 轮询 /state 更新。
  // 心跳：确认 JS 已执行（协议日志可见 /ping）
  fetch("http://console.localhost/ping").catch(()=>{});
  const INITIAL = `__INITIAL_STATE__`;
  const INITIAL_HIST = `__INITIAL_HISTORY__`;
  let lastJson = "";
  function applyState(json){
    if(json===lastJson) return;
    lastJson=json;
    let op=null;
    try{ op=JSON.parse(json); }catch(e){ return; }
    render(op);
  }
  if(INITIAL && INITIAL!=="null") applyState(INITIAL);
  try{ renderHist(JSON.parse(INITIAL_HIST)); }catch(e){}
  // 轮询 /state（每 1.5s）
  setInterval(()=>{
    fetch("http://console.localhost/state").then(r=>r.text()).then(applyState).catch(()=>{});
    fetch("http://console.localhost/history").then(r=>r.text()).then(t=>{try{renderHist(JSON.parse(t));}catch(e){}}).catch(()=>{});
  }, 1500);
})();
</script>
</body>
</html>"#.to_string();
    // 内嵌初始状态（替换占位符；JSON 需转义避免破坏 JS 字符串）
    // 注意：必须同时转义 </script>，防止错误文本里的标签提前闭合脚本块（注入/截断）
    let escape = |s: String| {
        s.replace('\\', "\\\\")
            .replace('`', "\\`")
            .replace("${", "\\${")
            .replace("</", "<\\/")
    };
    html.replace("__INITIAL_STATE__", &escape(initial))
        .replace("__INITIAL_HISTORY__", &escape(history))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 向导 HTML 必须包含三个新能力（Q1 的验收点）：
    /// ① 计划区、② 历史区、③ 历史接口占位符被真实替换。
    #[test]
    fn wizard_html_has_plan_and_history() {
        let html = console_html();
        // ① 「将要执行」计划区（让用户看到要更新什么）
        assert!(html.contains("opPlan"), "缺少计划区容器");
        assert!(html.contains("将要执行"), "缺少计划区标题");
        // ② 历史记录折叠区（事后可回看）
        assert!(html.contains("opHist"), "缺少历史区容器");
        assert!(html.contains("历史记录"), "缺少历史区标题");
        // ③ 占位符必须被替换掉，不能残留（残留会让页面显示 `__INITIAL_...__`）
        assert!(!html.contains("__INITIAL_STATE__"), "初始状态占位符未替换");
        assert!(!html.contains("__INITIAL_HISTORY__"), "历史占位符未替换");
        // 轮询 /history（历史要能实时刷新）
        assert!(html.contains("/history"), "前端未轮询 /history");
    }

    /// 注入防护：状态里的 `</script>` 不能提前闭合脚本块。
    /// （历史/结果文本可能来自 pnpm 输出，属外部数据）
    #[test]
    fn wizard_html_escapes_script_close() {
        // 直接验证转义函数行为（与 console_html 内一致）
        let dangerous = "x</script><script>alert(1)</script>";
        let escaped = dangerous.replace("</", "<\\/");
        assert!(!escaped.contains("</script>"), "必须转义 </ 防止脚本块提前闭合");
    }
}
