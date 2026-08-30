#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod admin_bridge;
mod cli;
mod commands;
mod config;
mod console;
mod download;
mod install;
mod logging;
mod mirror;
mod notify;
mod ops;
mod plugin;
mod speedtest;
mod sync;
mod tray;
mod workflow;

fn main() {
    // 解析 CLI 参数（--cmd 等）
    let cli_args = cli::parse_args();
    let cli_mode = cli::is_cli_mode(&cli_args);

    // CLI 模式：也需要 Tauri 初始化（路径/配置/下载依赖 AppHandle），
    // 但 build 后不 run 事件循环——执行命令后直接退出。
    if cli_mode {
        let app = tauri::Builder::default()
            .plugin(tauri_plugin_notification::init())
            .build(tauri::generate_context!())
            .expect("error building launcher (cli)");
        let handle = app.handle().clone();

        // 日志
        let log_path = config::log_file(&handle);
        logging::init(&log_path);
        config::ensure_base_dir(&handle);
        let _ = config::load_config(&handle);
        log::info!("CLI 模式：执行命令 {}", cli_args.cmd.as_deref().unwrap_or(""));

        let code = cli::run_cli(&handle, &cli_args);
        std::process::exit(code);
    }

    // 正常常驻模式
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        // 单实例：托盘常驻程序避免被二次启动。后启动的实例直接退出，
        // 已运行的实例保持不动（不重复启动 Harness，也不互杀进程树）。
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            log::info!("检测到重复启动，本次实例退出，已有实例继续运行");
            let _ = app;
        }))
        // IPC 命令（前端/测试脚本通过 invoke 调用常驻实例）
        .invoke_handler(tauri::generate_handler![
            commands::cmd_install,
            commands::cmd_launch,
            commands::cmd_stop,
            commands::cmd_sync,
            commands::cmd_speedtest,
            commands::cmd_mirror,
            commands::cmd_status,
            commands::cmd_open_console,
        ])
        // 操作进度窗口的内嵌 HTML 协议（console://localhost/index.html）
        // data: URL 在 Tauri 2 External 里被安全策略拦截，改用自定义协议。
        .register_uri_scheme_protocol("console", |_ctx, request| {
            use tauri::http::Response;
            // /state → 当前操作状态 JSON（窗口加载后拉取）
            let path = request.uri().path().to_string();
            log::info!("console:// 协议请求：{}", path);
            if path == "/ping" {
                log::info!("console:// JS 心跳：脚本已执行");
                return Response::builder()
                    .header("Content-Type", "text/plain")
                    .body("pong".into())
                    .unwrap_or_default();
            }
            if path == "/state" {
                let op = ops::current();
                let body = serde_json::to_string(&op).unwrap_or_else(|_| "null".to_string());
                // 日志截断必须按字符边界（body 含中文，直接字节切片会 panic）
                log::info!("console:// /state 返回：{}", crate::config::truncate_utf8(&body, 120));
                return Response::builder()
                    .header("Content-Type", "application/json; charset=utf-8")
                    .body(body.into_bytes())
                    .unwrap_or_default();
            }
            // 其他 → 内嵌 HTML
            let html = console::console_html();
            Response::builder()
                .header("Content-Type", "text/html; charset=utf-8")
                // 关键：允许内联脚本 + 本协议 fetch（Tauri 默认注入的 CSP 会拦内联 JS）
                .header(
                    "Content-Security-Policy",
                    "default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self' http://console.localhost",
                )
                .body(html.into_bytes())
                .unwrap_or_default()
        })
        .setup(|app| {
            let handle = app.handle().clone();

            // 日志（文件 + stdout）
            let log_path = config::log_file(&handle);
            logging::init(&log_path);

            // 配置
            config::ensure_base_dir(&handle);
            let cfg = config::load_config(&handle);
            log::info!("DeepSeek Harness Launcher 启动");

            // 托盘
            tray::build_tray(&handle)?;

            // 恢复上次操作状态（重启后托盘/窗口可见上次结果）
            ops::load_from_disk(&handle);

            // 自动启动 Harness（若配置开启且已安装）
            if cfg.auto_start.unwrap_or(false) && config::dsh_binary_path(&handle).exists() {
                let h = handle.clone();
                tauri::async_runtime::spawn(async move {
                    match workflow::launch(&h) {
                        Ok(port) => notify::notify(
                            &h,
                            "DeepSeek Harness",
                            &format!("已启动：http://127.0.0.1:{port}"),
                        ),
                        Err(e) => notify::notify(&h, "启动失败", &e),
                    }
                });
            }

            // IP 地域检测（异步，启动即触发；结果缓存后供加速源解析）
            {
                let h = handle.clone();
                tauri::async_runtime::spawn(async move {
                    let cfg = config::load_cached();
                    let _ = config::detect_region_async(&cfg).await;
                    let _ = h;
                });
            }

            // 企业中心服务端同步：配置了 serverUrl 才启用。
            // 离线容错：循环永不退出，失败仅日志，恢复后自动补拉。
            if !config::resolve_server_url(&cfg).is_empty() {
                let h = handle.clone();
                tauri::async_runtime::spawn(async move {
                    sync::spawn_sync_loop(&h).await;
                });
            }

            // 管理能力（外网代理网关）：配置开启则启动本地 API
            if config::bridge_enabled(&cfg) {
                match admin_bridge::start(&handle) {
                    Ok(p) => log::info!("管理能力已开启：http://127.0.0.1:{p}"),
                    Err(e) => log::warn!("管理能力启动失败：{e}"),
                }
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error building launcher")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                // 托盘「退出」菜单设置了标志 → 真退出（停 Harness + 退出）；
                // 否则（关闭进度窗口等）阻止退出，应用常驻托盘。
                if crate::tray::is_quit_requested() {
                    workflow::stop();
                } else {
                    api.prevent_exit();
                }
                let _ = app;
            }
        });
}
