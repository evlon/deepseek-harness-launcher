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

use tauri::{AppHandle, Manager, Runtime};

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
        let html = welcome_html(app);
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
            // 后台跑安装（复用 install_all，进度内嵌于向导窗口；install_all 已做
            // 「向导存在则不弹独立进度窗」）。先开向导再装——用户立刻看到单窗口，
            // 安装进度直接滚动，装完自动衔接激活，全程一个窗口。
            tauri::async_runtime::spawn(async move {
                // 先关首屏窗（避免 first-run + matrix-setup 双窗并存），单窗口收口到向导
                close_window(&h);
                // 先打开向导（用户立即看到界面 + 内嵌进度），再装再激活，全程单窗口
                let _ = crate::matrix_setup::open_window(&h);
                // 等安装完成
                let _ = crate::install::install_all(&h).await;
                // 安装已完成：立即触发向导内的自动激活（POST /resume-auto-activation），
                // 保证「装完即用」不需再点按钮选「开始激活」
                let _h2 = h.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    // 给向导一点热身时间打开
                    std::thread::sleep(std::time::Duration::from_millis(600));
                    // 通过本地 HTTP 协议调向导端点（不关心返回，纯触发）
                    let _ = reqwest::blocking::Client::new()
                        .post("http://matrix-setup.localhost/resume-auto-activation")
                        .send()
                        .ok();
                    let _ = _h2;
                });
                log::info!("首次安装完成，已在向导窗口内自动衔接激活流程");
            });
            return json_resp(
                StatusCode::OK,
                serde_json::json!({"ok": true, "message": "正在安装依赖，完成后将自动激活数字分身"}),
            );
        }
        // dsh 已装 → 先确保 matrix profile 骨架 + 推荐插件就绪，再打开激活向导。
        // 背景（2026-09-23 同事故障）：旧实现直接 open_window，跳过 install_all，
        // 导致「dsh 已装但 matrix profile 从未创建」时，激活流程 launch 报
        // "profile does not exist" → HARNESS_NOT_READY。
        tauri::async_runtime::spawn(async move {
            let cfg = load_cached();
            // 先关首屏窗（避免 first-run + matrix-setup 双窗并存），单窗口收口到向导
            close_window(&h);
            // 1) 同步建 matrix profile 骨架（manifest + bundles + 品牌 patch + env defaults）
            if let Err(e) = crate::install::ensure_matrix_profile(&h, &cfg) {
                log::warn!("激活前确保 matrix profile 骨架失败：{e}");
            }
            // 2) 异步装 matrix profile 推荐插件（dsh-matrix-agent 等，幂等，已装则跳过）
            if let Err(e) = crate::install::install_server_recommended(&h, &cfg).await {
                log::warn!("激活前装 matrix 推荐插件未完成（可在激活后托盘补装）：{e}");
            }
            // 2️⃣ 直接打开「数字分身配置向导」单窗口处理全部流程（安装进度、激活、岗位选择），不再中途关闭重开
            // ⚠️ 不再 open_console 预热独立进度窗——进度统一内嵌在向导窗口内（见 wizard_html 的
            //    进度区 + /op-state 轮询），避免「配置数字分身 + 操作进度」双窗交替闪烁。
            let _ = crate::matrix_setup::open_window(&h);
            // 已装未激活：同样直接触发自动激活，免去用户再点按钮
            let _h2 = h.clone();
            tauri::async_runtime::spawn_blocking(move || {
                std::thread::sleep(std::time::Duration::from_millis(500));
                let _ = reqwest::blocking::Client::new()
                    .post("http://matrix-setup.localhost/resume-auto-activation")
                    .send()
                    .ok();
            });
        });
        return json_resp(
            StatusCode::OK,
            serde_json::json!({"ok": true, "message": "正在准备数字分身运行环境，随后进入激活"}),
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
fn welcome_html<R: Runtime>(app: &AppHandle<R>) -> String {
    // 双轨：服务端下发版优先（同步拉取，本地缓存），离线回落编译期内嵌
    crate::embedded::first_run_html(app)
}
