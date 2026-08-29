//! 系统托盘：常驻任务栏的入口。包含安装 / 启动 / 停止 / 加速 / 网址 / 同步菜单。

use tauri::menu::{Menu, MenuItem, Submenu};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Runtime};
use tauri_plugin_opener::OpenerExt;

use crate::config::*;
use crate::notify::notify;

const TRAY_ID: &str = "main-tray";

/// 构建托盘图标与菜单并挂载事件。
pub fn build_tray<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    // 直接内嵌 PNG（无窗口应用没有默认窗口图标，避免 unwrap 恐慌）。
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/icon.png"))?;

    let menu = build_menu(app)?;

    let _ = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip("DeepSeek Harness Launcher")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| handle_menu_event(app, event))
        .on_tray_icon_event(|tray, event| {
            // 注意：不要在 Click 事件里重建菜单（Windows 右键弹出菜单的同时
            // set_menu 会导致菜单弹不出来）。菜单状态在操作完成后刷新即可。
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                ..
            } = event
            {
                // 左键点击打开 Harness 页面（仅在服务运行时）
                if crate::workflow::is_running() {
                    if let Some(port) = crate::workflow::last_port() {
                        let _ = tray.app_handle().opener().open_url(
                            format!("http://127.0.0.1:{port}"),
                            None::<&str>,
                        );
                    }
                }
            }
        })
        .build(app)?;

    Ok(())
}

/// 刷新托盘菜单（同步完成后调用，让「推荐插件」子菜单反映最新状态）。
pub fn refresh_sync_menu<R: Runtime>(app: &AppHandle<R>) {
    let Ok(menu) = build_menu(app) else {
        log::warn!("重建托盘菜单失败");
        return;
    };
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        if let Err(e) = tray.set_menu(Some(menu)) {
            log::warn!("更新托盘菜单失败：{e}");
        }
    }
}

/// 组装完整菜单（静态项 + 动态「推荐插件」子菜单）。
fn build_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    // 常用网址子菜单：服务端菜单策略启用 → 策略项；否则用户本地项
    let cfg = load_cached();
    let links = crate::sync::current_menu(app, &cfg);
    let link_items: Vec<MenuItem<R>> = if links.is_empty() {
        vec![MenuItem::with_id(app, "link-none", "（未配置）", false, None::<&str>)?]
    } else {
        links
            .iter()
            .enumerate()
            .map(|(i, _)| {
                MenuItem::with_id(app, format!("link-{i}"), links[i].label.clone(), true, None::<&str>)
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    let url_items: Vec<&dyn tauri::menu::IsMenuItem<R>> = link_items
        .iter()
        .map(|m| m as &dyn tauri::menu::IsMenuItem<R>)
        .collect();
    let url_submenu = Submenu::with_id_and_items(app, "urls", "常用网址", true, &url_items)?;

    // 加速 ▸ npm 源 / GitHub 中转
    let npm_submenu = Submenu::with_id_and_items(
        app,
        "npm",
        "npm 源",
        true,
        &[
            &MenuItem::with_id(app, "npm-auto", "自动（按地域）", true, None::<&str>)?,
            &MenuItem::with_id(app, "npm-official", "官方源 npmjs.org", true, None::<&str>)?,
            &MenuItem::with_id(app, "npm-npmmirror", "npmmirror 镜像", true, None::<&str>)?,
        ],
    )?;
    let gh_submenu = Submenu::with_id_and_items(
        app,
        "gh",
        "GitHub 中转",
        true,
        &[
            &MenuItem::with_id(app, "gh-auto", "自动（按地域）", true, None::<&str>)?,
            &MenuItem::with_id(app, "gh-none", "直连（无中转）", true, None::<&str>)?,
            &MenuItem::with_id(app, "gh-ghfast", "ghfast.top 中转", true, None::<&str>)?,
        ],
    )?;
    // 加速 ▸ npm 源 / GitHub 中转 / 测速
    let speedtest_item = MenuItem::with_id(app, "accel-speedtest", "测速（探测各源延迟）", true, None::<&str>)?;
    let accel_submenu = Submenu::with_id_and_items(
        app,
        "accel",
        "加速设置",
        true,
        &[&npm_submenu, &gh_submenu, &speedtest_item],
    )?;

    // 同步 / 推荐插件 子菜单（动态）
    let sync_submenu = build_sync_submenu(app)?;
    // 切换 Profile 子菜单（动态）
    let profile_submenu = build_profile_submenu(app)?;
    // 管理能力子菜单（动态）
    let bridge_submenu = build_bridge_submenu(app)?;

    // 操作状态区（动态）：有进行中/最近操作时显示在菜单顶部
    // 先创建所有 owned 菜单项，再收集引用（避免临时值借用问题）
    let mut owned: Vec<MenuItem<R>> = Vec::new();
    if let Some(op) = crate::ops::current() {
        if op.state != crate::ops::OpState::Idle {
            // 状态行（禁用项，展示当前步骤/结果）
            let status_text = match op.state {
                crate::ops::OpState::Running => format!("⏳ {}：{}", op.label, op.current_step),
                crate::ops::OpState::Done => format!("✓ {} 完成", op.label),
                crate::ops::OpState::Failed => format!("✗ {} 失败", op.label),
                crate::ops::OpState::Idle => String::new(),
            };
            if !status_text.is_empty() {
                owned.push(MenuItem::with_id(app, "op-status", status_text, false, None::<&str>)?);
            }
            // 查看进度（运行中或完成/失败都可看日志）
            owned.push(MenuItem::with_id(app, "op-view", "📋 查看进度 / 日志", true, None::<&str>)?);
        }
    }
    // Harness 运行状态：菜单项按状态动态可用
    // 运行中 → 只能「停止」「打开页面」；未运行 → 只能「启动」
    let running = crate::workflow::is_running();
    let launch_enabled = !running && !crate::ops::has_running();
    let stop_enabled = running;
    let open_page_enabled = running;
    let install_enabled = !crate::ops::has_running();

    owned.push(MenuItem::with_id(app, "install", "安装 / 修复", install_enabled, None::<&str>)?);
    owned.push(MenuItem::with_id(app, "launch", "启动 Harness", launch_enabled, None::<&str>)?);
    owned.push(MenuItem::with_id(app, "open-page", "打开 Harness 页面", open_page_enabled, None::<&str>)?);
    owned.push(MenuItem::with_id(app, "stop", "停止 Harness", stop_enabled, None::<&str>)?);
    owned.push(MenuItem::with_id(app, "log", "查看日志", true, None::<&str>)?);
    owned.push(MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?);

    // 固定子菜单（引用）
    let mut items: Vec<&dyn tauri::menu::IsMenuItem<R>> = owned
        .iter()
        .map(|m| m as &dyn tauri::menu::IsMenuItem<R>)
        .collect();
    items.push(&profile_submenu);
    items.push(&bridge_submenu);
    items.push(&accel_submenu);
    items.push(&sync_submenu);
    items.push(&url_submenu);

    let menu = Menu::with_items(app, &items)?;
    Ok(menu)
}

/// 构建「管理能力」子菜单：外网代理网关开关 + 状态。
fn build_bridge_submenu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Submenu<R>> {
    let running = crate::admin_bridge::is_running();
    let mut items: Vec<&dyn tauri::menu::IsMenuItem<R>> = Vec::new();
    let mut rows: Vec<MenuItem<R>> = Vec::new();

    if running {
        let port = crate::admin_bridge::port().unwrap_or(0);
        rows.push(MenuItem::with_id(
            app,
            "bridge-stop",
            format!("关闭管理能力（http://127.0.0.1:{port}）"),
            true,
            None::<&str>,
        )?);
    } else {
        rows.push(MenuItem::with_id(
            app,
            "bridge-start",
            "开启管理能力（外网代理）".to_string(),
            true,
            None::<&str>,
        )?);
    }
    let status = if running {
        format!("状态：运行中（端口 {}）", crate::admin_bridge::port().unwrap_or(0))
    } else {
        "状态：未开启".to_string()
    };
    rows.push(MenuItem::with_id(app, "bridge-status", status, false, None::<&str>)?);
    items.extend(rows.iter().map(|m| m as &dyn tauri::menu::IsMenuItem<R>));

    Submenu::with_id_and_items(app, "bridge", "管理能力", true, &items)
}

/// 构建「切换 Profile」子菜单：枚举已装 profile，当前运行项标记 ✓。
fn build_profile_submenu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Submenu<R>> {
    let cfg = load_cached();
    let profiles = list_profiles(app, &cfg);
    let current = crate::workflow::current_profile();
    let configured = resolve_profile(&cfg);

    let mut items: Vec<&dyn tauri::menu::IsMenuItem<R>> = Vec::new();
    let mut profile_items: Vec<MenuItem<R>> = Vec::new();
    let mut none_items: Vec<MenuItem<R>> = Vec::new();

    if profiles.is_empty() {
        none_items.push(MenuItem::with_id(app, "profile-none", "（无已安装 profile，请先「安装 / 修复」）", false, None::<&str>)?);
        items.extend(none_items.iter().map(|m| m as &dyn tauri::menu::IsMenuItem<R>));
    } else {
        for (i, name) in profiles.iter().enumerate() {
            let running = current.as_deref() == Some(name.as_str());
            let is_configured = configured == *name;
            let label = if running {
                format!("{name}  ✓（运行中）")
            } else if is_configured {
                format!("{name}（默认）")
            } else {
                name.clone()
            };
            profile_items.push(MenuItem::with_id(app, format!("profile-{i}"), label, true, None::<&str>)?);
        }
        items.extend(profile_items.iter().map(|m| m as &dyn tauri::menu::IsMenuItem<R>));
    }

    Submenu::with_id_and_items(app, "profiles", "切换 Profile", true, &items)
}

/// 构建「同步 / 推荐插件」子菜单：待装推荐各一条「安装 X」+ 立即同步。
fn build_sync_submenu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Submenu<R>> {
    let cfg = load_cached();
    let enabled = !resolve_server_url(&cfg).is_empty();
    if !enabled {
        return Submenu::with_id_and_items(
            app,
            "sync",
            "同步 / 推荐插件",
            true,
            &[&MenuItem::with_id(app, "sync-none", "未配置服务端（launcher-config.json 的 serverUrl）", false, None::<&str>)?],
        );
    }

    // 待装清单：优先用缓存的服务端配置（离线也可显示），实时拉取由同步循环负责
    let installed = crate::sync::installed_plugins(app, &cfg);
    let state = crate::sync::load_state(app, &cfg);
    let pending: Vec<String> = state
        .cached_config
        .as_ref()
        .map(|c| crate::sync::pending_plugins(&c.plugins, &installed))
        .unwrap_or_default();

    let mut status_items: Vec<MenuItem<R>> = Vec::new();
    let mut install_items: Vec<MenuItem<R>> = Vec::new();

    if pending.is_empty() {
        status_items.push(MenuItem::with_id(app, "sync-uptodate", "已是最新（无待装推荐）", false, None::<&str>)?);
    } else {
        for (i, name) in pending.iter().enumerate() {
            install_items.push(MenuItem::with_id(
                app,
                format!("sync-install-{i}"),
                format!("安装 {name}"),
                true,
                None::<&str>,
            )?);
        }
    }

    let mut items: Vec<&dyn tauri::menu::IsMenuItem<R>> = Vec::new();
    items.extend(status_items.iter().map(|m| m as &dyn tauri::menu::IsMenuItem<R>));
    items.extend(install_items.iter().map(|m| m as &dyn tauri::menu::IsMenuItem<R>));
    // 立即同步（手动触发）
    let refresh = MenuItem::with_id(app, "sync-now", "立即同步", true, None::<&str>)?;
    items.push(&refresh);

    Submenu::with_id_and_items(app, "sync", "同步 / 推荐插件", true, &items)
}

/// 供菜单点击时取「第 i 个待装插件名」（与 build_sync_submenu 的索引一致）。
fn pending_plugin_at<R: Runtime>(app: &AppHandle<R>, index: usize) -> Option<String> {
    let cfg = load_cached();
    let installed = crate::sync::installed_plugins(app, &cfg);
    let state = crate::sync::load_state(app, &cfg);
    state
        .cached_config
        .as_ref()
        .and_then(|c| crate::sync::pending_plugins(&c.plugins, &installed).get(index).cloned())
}

fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, event: tauri::menu::MenuEvent) {
    match event.id().as_ref() {
        "install" => {
            // 防重复安装：已有进行中的长操作则拒绝
            if crate::ops::has_running() {
                notify(app, "安装 / 修复", "已有操作进行中，请稍候");
                return;
            }
            let h = app.clone();
            tauri::async_runtime::spawn(async move {
                // install_all 内部：登记操作 + 弹窗 + 分步通知 + 完成/失败 ops
                let _ = crate::install::install_all(&h).await;
                refresh_sync_menu(&h);
            });
        }
        "launch" => {
            let h = app.clone();
            tauri::async_runtime::spawn(async move {
                crate::ops::start_op(&h, "launch", "启动 Harness", &[]);
                match crate::workflow::launch(&h) {
                    Ok(port) => {
                        crate::ops::finish_op(&h, &format!("已启动，访问 http://127.0.0.1:{port}"));
                        notify(&h, "Harness 已启动", &format!("访问 http://127.0.0.1:{port}"));
                        refresh_sync_menu(&h);
                    }
                    Err(e) => {
                        crate::ops::fail_op(&h, &e);
                        notify(&h, "启动失败", &e);
                        refresh_sync_menu(&h);
                    }
                }
            });
        }
        "open-page" => {
            // 以实际运行端口为准；未运行则尝试配置端口（可能尚未拉起，保持原行为）
            let port = crate::workflow::last_port()
                .unwrap_or_else(|| resolve_port(&load_cached()));
            let _ = app.opener().open_url(format!("http://127.0.0.1:{port}"), None::<&str>);
        }
        "stop" => {
            crate::workflow::stop();
            crate::ops::start_op(app, "stop", "停止 Harness", &[]);
            crate::ops::finish_op(app, "Harness 已停止");
            notify(app, "Harness 已停止", "");
            refresh_sync_menu(app);
        }
        id if id.starts_with("profile-") => {
            // 切换 Profile：切到目标 profile 并启动
            let idx = id
                .strip_prefix("profile-")
                .and_then(|s| s.parse::<usize>().ok());
            if let Some(idx) = idx {
                let cfg = load_cached();
                let profiles = list_profiles(app, &cfg);
                if let Some(name) = profiles.get(idx) {
                    let name = name.clone();
                    let h = app.clone();
                    tauri::async_runtime::spawn(async move {
                        // 记录默认 profile 并切换启动
                        crate::ops::start_op(&h, "profile", "切换 Profile", &[]);
                        let _ = set_profile(&h, &name);
                        match crate::workflow::launch_with_profile(&h, &name) {
                            Ok(port) => {
                                crate::ops::finish_op(&h, &format!("{name}：http://127.0.0.1:{port}"));
                                notify(&h, "Profile 已切换", &format!("{name}：http://127.0.0.1:{port}"));
                                refresh_sync_menu(&h);
                            }
                            Err(e) => {
                                crate::ops::fail_op(&h, &e);
                                notify(&h, "Profile 切换失败", &e);
                                refresh_sync_menu(&h);
                            }
                        }
                    });
                }
            }
        }
        "op-view" => {
            if let Err(e) = crate::console::open_console(app) {
                notify(app, "无法打开进度窗口", &e);
            }
        }
        "bridge-start" => {
            let h = app.clone();
            tauri::async_runtime::spawn(async move {
                crate::ops::start_op(&h, "bridge", "开启管理能力", &[]);
                match crate::admin_bridge::start(&h) {
                    Ok(port) => {
                        let _ = set_bridge_enabled(&h, true);
                        crate::ops::finish_op(&h, &format!("本地 API：http://127.0.0.1:{port}"));
                        notify(&h, "管理能力已开启", &format!("本地 API：http://127.0.0.1:{port}"));
                        refresh_sync_menu(&h);
                    }
                    Err(e) => {
                        crate::ops::fail_op(&h, &e);
                        notify(&h, "管理能力开启失败", &e);
                        refresh_sync_menu(&h);
                    }
                }
            });
        }
        "bridge-stop" => {
            crate::admin_bridge::stop();
            let _ = set_bridge_enabled(app, false);
            crate::ops::start_op(app, "bridge", "关闭管理能力", &[]);
            crate::ops::finish_op(app, "管理能力已关闭");
            notify(app, "管理能力已关闭", "");
            refresh_sync_menu(app);
        }
        "sync-now" => {
            notify(app, "同步", "正在与中心服务端同步…");
            let h = app.clone();
            tauri::async_runtime::spawn(async move {
                crate::ops::start_op(&h, "sync", "同步", &["拉取服务端配置", "执行策略"]);
                crate::ops::mark_step_running(&h, 0);
                crate::ops::update_step(&h, "正在拉取服务端配置…");
                let cfg = load_cached();
                let outcome = crate::sync::sync_once(&h, &cfg, None).await;
                crate::ops::mark_step_running(&h, 1);
                crate::ops::update_step(&h, "执行策略…");
                refresh_sync_menu(&h);
                if let Some(config) = &outcome.config {
                    let msg = if outcome.pending.is_empty() {
                        "已是最新，无待安装推荐插件".to_string()
                    } else {
                        format!("待安装推荐：{}", outcome.pending.join(", "))
                    };
                    crate::ops::finish_op(&h, &msg);
                    notify(&h, "同步完成", &msg);
                    let _ = config;
                } else {
                    crate::ops::fail_op(&h, "无法连接中心服务端（已使用本地缓存）");
                    notify(&h, "同步失败", "无法连接中心服务端（已使用本地缓存）");
                }
            });
        }
        id if id.starts_with("sync-install-") => {
            let idx = id
                .strip_prefix("sync-install-")
                .and_then(|s| s.parse::<usize>().ok());
            if let Some(idx) = idx {
                if let Some(name) = pending_plugin_at(app, idx) {
                    let h = app.clone();
                    let name_clone = name.clone();
                    tauri::async_runtime::spawn(async move {
                        crate::ops::start_op(&h, "plugin-install", "安装插件", &["安装插件"]);
                        crate::ops::mark_step_running(&h, 0);
                        crate::ops::update_step(&h, &format!("正在安装 {name_clone}…"));
                        match crate::sync::install_plugin(&h, &name).await {
                            Ok(()) => {
                                crate::ops::finish_op(&h, &format!("{} 已就绪", name_clone));
                                notify(&h, "插件已安装", &format!("{} 已就绪", name_clone));
                                // 安装后立即同步一次（刷新状态 + 上报服务端）
                                let cfg = load_cached();
                                let _ = crate::sync::sync_once(&h, &cfg, None).await;
                                refresh_sync_menu(&h);
                            }
                            Err(e) => {
                                crate::ops::fail_op(&h, &e);
                                notify(&h, "插件安装失败", &e);
                                refresh_sync_menu(&h);
                            }
                        }
                    });
                }
            }
        }
        "npm-auto" => apply_accel(app, "npm", ""),
        "npm-official" => apply_accel(app, "npm", "https://registry.npmjs.org/"),
        "npm-npmmirror" => apply_accel(app, "npm", "https://registry.npmmirror.com/"),
        "gh-auto" => apply_accel(app, "gh", ""),
        "gh-none" => apply_accel(app, "gh", "none"),
        "gh-ghfast" => apply_accel(app, "gh", "https://ghfast.top/"),
        "accel-speedtest" => {
            notify(app, "测速", "正在探测各加速源延迟，请稍候…");
            let h = app.clone();
            tauri::async_runtime::spawn(async move {
                crate::ops::start_op(&h, "speedtest", "加速源测速", &["探测 npm 源", "探测 GitHub 中转"]);
                crate::ops::mark_step_running(&h, 0);
                crate::ops::update_step(&h, "探测 npm 源…");
                let cfg = load_cached();
                let npm = crate::speedtest::speedtest_npm(&h, &cfg).await;
                crate::ops::mark_step_running(&h, 1);
                crate::ops::update_step(&h, "探测 GitHub 中转…");
                let gh = crate::speedtest::speedtest_gh(&h, &cfg).await;

                let mut lines: Vec<String> = Vec::new();
                lines.push("── npm 源 ──".to_string());
                for r in &npm {
                    let mark = if r.ok { "✓" } else { "✗" };
                    lines.push(format!("{mark} {} {}ms", r.name, r.latency_ms));
                }
                lines.push("── GitHub 中转 ──".to_string());
                for r in &gh {
                    let mark = if r.ok { "✓" } else { "✗" };
                    lines.push(format!("{mark} {} {}ms", r.name, r.latency_ms));
                }
                let msg = lines.join("\n");
                // 结果可能较长，截断到通知上限
                let msg = if msg.len() > 900 { format!("{}…", &msg[..900]) } else { msg };
                crate::ops::finish_op(&h, "测速完成（详见结果通知）");
                notify(&h, "加速源测速结果", &msg);
                refresh_sync_menu(&h);
            });
        }
        "log" => {
            let path = log_file(app);
            let _ = app.opener().open_path(path.to_string_lossy().to_string(), None::<&str>);
        }
        id if id.starts_with("link-") => {
            if let Some(idx_str) = id.strip_prefix("link-") {
                if let Ok(idx) = idx_str.parse::<usize>() {
                    let cfg = load_cached();
                    if let Some(link) = crate::sync::current_menu(app, &cfg).get(idx) {
                        let _ = app.opener().open_url(link.url.clone(), None::<&str>);
                    }
                }
            }
        }
        "quit" => {
            crate::workflow::stop();
            app.exit(0);
        }
        _ => {}
    }
}

/// 应用加速预设：写入配置并重应用 npmrc。
fn apply_accel<R: Runtime>(app: &AppHandle<R>, kind: &str, value: &str) {
    let result = if kind == "npm" {
        set_npm_registry(app, value)
    } else {
        let prefix = if value == "none" { None } else { Some(value) };
        set_gh_prefix(app, prefix)
    };
    match result {
        Ok(()) => {
            let cfg = load_cached();
            let _ = crate::plugin::ensure_profile_npmrc(app, &cfg);
            let msg = if kind == "npm" {
                format!("npm 源已设为：{}", resolve_npm_registry(&cfg))
            } else {
                match resolve_gh_prefix(&cfg) {
                    Some(p) => format!("GitHub 中转已设为：{p}"),
                    None => "GitHub 中转：直连（无中转）".to_string(),
                }
            };
            notify(app, "加速设置已更新", &msg);
        }
        Err(e) => notify(app, "加速设置失败", &e),
    }
}
