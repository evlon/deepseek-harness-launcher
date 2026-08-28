#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod admin_bridge;
mod config;
mod download;
mod install;
mod logging;
mod notify;
mod plugin;
mod sync;
mod tray;
mod workflow;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        // 单实例：托盘常驻程序避免被二次启动。后启动的实例直接退出，
        // 已运行的实例保持不动（不重复启动 Harness，也不互杀进程树）。
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            log::info!("检测到重复启动，本次实例退出，已有实例继续运行");
            let _ = app;
        }))
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
            if let tauri::RunEvent::ExitRequested { .. } = event {
                workflow::stop();
                let _ = app;
            }
        });
}
