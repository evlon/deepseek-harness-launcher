//! 内嵌 UI 静态资源统一入口。
//!
//! 三个窗口（操作进度 / 首次欢迎 / 数字分身激活向导）的内嵌 HTML
//! 已从 Rust 字符串抽为独立静态资源（`src-tauri/embedded-ui/*.html`），
//! 编译期内嵌为**离线兜底**；通过 `ui_bundle.rs` 实现「服务端下发版优先」双轨加载：
//!
//! ```text
//! 窗口 HTML = ui_bundle::load_html(缓存版)  ??  内置
//! ```
//!
//! ## 设计目标（三步渐进）
//! 1. 第一步（已完成）：UI 与能力分离——HTML 不再是 Rust 字符串，改 UI 更易维护
//!    （仍需重编译发版）。
//! 2. 第二步（本步）：双轨加载——窗口优先加载服务端下发的 UI 包（同步时拉取到本地
//!    缓存），离线/首次回落内置；**UI 迭代走服务端发版，exe 只在能力变更时发版**。
//! 3. 第三步（后续）：浏览器控制台——同一套 UI 挂到 conf.ai.ict.cmcc 可浏览器直开，
//!    能力仍走本地 bridge 端点 + 短 token。
//!
//! 注意：占位符替换（`__INITIAL_STATE__` 等）由各窗口 handler 做，与 HTML 来源无关，
//! 因此「内置 ⇄ 服务端下发」切换不需要改动任何替换逻辑。

use tauri::{AppHandle, Runtime};

/// 操作进度窗口 HTML（console:// 协议）。双轨：缓存优先，内置兜底。
pub fn console_html<R: Runtime>(app: &AppHandle<R>) -> String {
    let builtin = console_html_inner();
    let (html, _src) = crate::ui_bundle::load_html(app, "console.html", &builtin);
    html
}

/// 首次欢迎窗口 HTML（first-run:// 协议）。双轨：缓存优先，内置兜底。
pub fn first_run_html<R: Runtime>(app: &AppHandle<R>) -> String {
    let builtin = first_run_html_inner();
    let (html, _src) = crate::ui_bundle::load_html(app, "first-run.html", &builtin);
    html
}

/// 数字分身激活向导 HTML（matrix-setup:// 协议）。双轨：缓存优先，内置兜底。
pub fn matrix_setup_html<R: Runtime>(app: &AppHandle<R>) -> String {
    let builtin = matrix_setup_html_inner();
    let (html, _src) = crate::ui_bundle::load_html(app, "matrix-setup.html", &builtin);
    html
}

/// 纯内置版（离线兜底 / 单元测试用）。不依赖 AppHandle。
pub fn console_html_inner() -> String {
    include_str!("../embedded-ui/console.html").to_string()
}

/// 纯内置版（离线兜底 / 单元测试用）。不依赖 AppHandle。
pub fn first_run_html_inner() -> String {
    include_str!("../embedded-ui/first-run.html").to_string()
}

/// 纯内置版（离线兜底 / 单元测试用）。不依赖 AppHandle。
pub fn matrix_setup_html_inner() -> String {
    include_str!("../embedded-ui/matrix-setup.html").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_html_files_complete() {
        // 三个 HTML 必须完整（以 </html> 结尾），防止抽取时截断
        assert!(console_html_inner().trim_end().ends_with("</html>"),
            "console.html 必须以 </html> 结尾");
        assert!(first_run_html_inner().trim_end().ends_with("</html>"),
            "first-run.html 必须以 </html> 结尾");
        assert!(matrix_setup_html_inner().trim_end().ends_with("</html>"),
            "matrix-setup.html 必须以 </html> 结尾");
    }

    #[test]
    fn embedded_html_has_key_placeholders() {
        // console 的占位符仍在（替换逻辑依赖它们）
        assert!(console_html_inner().contains("__INITIAL_STATE__"));
        assert!(console_html_inner().contains("__INITIAL_HISTORY__"));
        // 向导 HTML 的关键能力点仍在
        assert!(matrix_setup_html_inner().contains("submitBtn"));
        assert!(first_run_html_inner().contains("开始激活"));
    }
}
