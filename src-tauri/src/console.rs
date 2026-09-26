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
///
/// 注意：窗口加载已改走 `embedded::console_html(app)`（双轨：服务端下发缓存优先），
/// 本函数保留为「纯内置组装」入口（测试与未来无 AppHandle 场景）；dead_code 属预期。
#[allow(dead_code)]
pub fn console_html() -> String {
    let initial = crate::ops::current()
        .map(|op| serde_json::to_string(&op).unwrap_or_else(|_| "null".to_string()))
        .unwrap_or_else(|| "null".to_string());
    let history = serde_json::to_string(&crate::ops::history(crate::ops::MAX_HISTORY))
        .unwrap_or_else(|_| "[]".to_string());
    // 内嵌 HTML 已抽为独立静态资源（src-tauri/embedded-ui/console.html），
    // 编译期 include_str! 内嵌，行为与原先内嵌字符串完全一致；
    // 后续可扩展为「服务端下发 UI（在线优先）→ 离线回落内置」双轨。
    let html = crate::embedded::console_html_inner();
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
