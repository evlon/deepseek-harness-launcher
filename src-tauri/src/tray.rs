//! 系统托盘：常驻任务栏的入口。包含安装 / 启动 / 停止 / 加速 / 网址 / 同步菜单。

use tauri::menu::{Menu, MenuItem, Submenu};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Runtime};
use tauri_plugin_opener::OpenerExt;

use crate::config::*;
use crate::notify::notify;

const TRAY_ID: &str = "main-tray";

/// 退出标志：托盘「退出」设置后，ExitRequested 才真正退出。
/// 关闭进度窗口等触发的 ExitRequested 会被阻止（应用常驻托盘）。
static QUIT_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 是否用户主动点了「退出」。
pub fn is_quit_requested() -> bool {
    QUIT_REQUESTED.load(std::sync::atomic::Ordering::Relaxed)
}

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

    // 加速 ▸ npm 源 / GitHub 中转（动态：常用源预设 + 当前选择标记 + 测速结果）
    // npm 源子菜单（测速后显示各源延迟）
    let npm_rows: Vec<MenuItem<R>> = NPM_REGISTRY_PRESETS
        .iter()
        .enumerate()
        .map(|(i, (label, url))| {
            let id = format!("npm-preset-{i}");
            // 当前选中的源打 ✓
            let active = resolve_npm_registry(&load_cached()) == resolve_preset_url(*url);
            // 测速结果显示延迟（自动/空地址不显示）
            let speed = if url.is_empty() {
                None
            } else {
                crate::speedtest::latency_for(url)
            };
            let mut text = label.to_string();
            if let Some(ms) = speed {
                text.push_str(&format!("  {ms}ms"));
            }
            if active && !url.is_empty() {
                text.push_str("  ✓");
            }
            MenuItem::with_id(app, id, text, true, None::<&str>)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let npm_refs: Vec<&dyn tauri::menu::IsMenuItem<R>> = npm_rows
        .iter()
        .map(|m| m as &dyn tauri::menu::IsMenuItem<R>)
        .collect();
    let npm_submenu = Submenu::with_id_and_items(app, "npm", "npm 源", true, &npm_refs)?;

    // GitHub 中转子菜单（测速后显示各镜像延迟）
    let gh_rows: Vec<MenuItem<R>> = GH_MIRROR_PRESETS
        .iter()
        .enumerate()
        .map(|(i, (label, url))| {
            let id = format!("gh-preset-{i}");
            // 测速结果显示延迟（自动/直连不显示）
            let speed = if url.is_empty() || *url == "none" {
                None
            } else {
                crate::speedtest::latency_for(url)
            };
            let mut text = label.to_string();
            if let Some(ms) = speed {
                text.push_str(&format!("  {ms}ms"));
            }
            MenuItem::with_id(app, id, text, true, None::<&str>)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let gh_refs: Vec<&dyn tauri::menu::IsMenuItem<R>> = gh_rows
        .iter()
        .map(|m| m as &dyn tauri::menu::IsMenuItem<R>)
        .collect();
    let gh_submenu = Submenu::with_id_and_items(app, "gh", "GitHub 中转", true, &gh_refs)?;
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
    // dsh 版本子菜单（动态：当前版本 + 已装版本切换 + 检查更新）
    let dsh_submenu = build_dsh_version_submenu(app)?;

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

    // 菜单顺序（按使用频率 + 逻辑分组）：
    //   操作状态区（动态）
    //   ── 常用 ──
    //   启动/停止（互斥可用）→ 打开页面 → 安装/修复
    //   ── 配置 ──
    //   切换 Profile → 同步/推荐插件 → 常用网址 → 加速设置
    //   ── 高级 ──
    //   管理能力 → 查看日志 → 退出
    // 菜单顺序（按使用频率 + 逻辑分组）：
    //   [操作状态区（动态）]
    //   ── 常用操作 ──
    //   启动/停止（状态互斥可用）→ 打开页面 → 安装/修复
    //   ── 配置 ──
    //   切换 Profile → 同步/推荐插件 → 常用网址 → 加速设置
    //   ── 高级 ──
    //   管理能力 → 查看日志 → 退出
    owned.push(MenuItem::with_id(app, "install", "安装 / 修复", install_enabled, None::<&str>)?);
    owned.push(MenuItem::with_id(app, "launch", "启动 Harness", launch_enabled, None::<&str>)?);
    owned.push(MenuItem::with_id(app, "open-page", "打开 Harness 页面", open_page_enabled, None::<&str>)?);
    owned.push(MenuItem::with_id(app, "stop", "停止 Harness", stop_enabled, None::<&str>)?);

    // 固定子菜单（引用）——按分组顺序
    let mut items: Vec<&dyn tauri::menu::IsMenuItem<R>> = owned
        .iter()
        .map(|m| m as &dyn tauri::menu::IsMenuItem<R>)
        .collect();
    // 配置组
    items.push(&profile_submenu);
    items.push(&sync_submenu);
    items.push(&url_submenu);
    items.push(&accel_submenu);
    // 高级组
    items.push(&dsh_submenu);
    items.push(&bridge_submenu);

    // 收尾：查看日志 / 退出
    let log_item = MenuItem::with_id(app, "log", "查看日志", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    items.push(&log_item);
    items.push(&quit_item);

    let menu = Menu::with_items(app, &items)?;
    Ok(menu)
}

/// 构建「dsh 版本」子菜单：当前版本 + 检查更新 + 已装版本切换 + 远程可安装版本。
fn build_dsh_version_submenu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Submenu<R>> {
    let mut rows: Vec<MenuItem<R>> = Vec::new();
    let active = crate::dsh_versions::active_version(app);
    let installed = crate::dsh_versions::list_installed(app);
    let active_tag = crate::dsh_versions::active_tag(app);

    // 状态行：当前版本
    let status_text = if active.is_empty() {
        "dsh 未安装".to_string()
    } else {
        format!("当前 dsh：{active}")
    };
    rows.push(MenuItem::with_id(app, "dsh-status", status_text, false, None::<&str>)?);
    // 检查更新（点击触发异步查询）
    rows.push(MenuItem::with_id(app, "dsh-check-update", "🔍 检查更新", true, None::<&str>)?);

    // 远程可安装版本（「检查更新」后缓存；点击即下载安装；已装的自动过滤）
    let remote = crate::dsh_versions::installable_remote_releases(app);
    let remote_rows: Vec<MenuItem<R>> = remote
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let version = r["version"].as_str().unwrap_or("").to_string();
            let prerelease = r["prerelease"].as_bool().unwrap_or(false);
            let label = if prerelease {
                format!("📥 安装 {version}（预发布）")
            } else {
                format!("📥 安装 {version}")
            };
            MenuItem::with_id(app, format!("dsh-install-{i}"), label, true, None::<&str>)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !remote_rows.is_empty() {
        rows.push(MenuItem::with_id(app, "dsh-remote-sep", "─ 远程可用版本 ─", false, None::<&str>)?);
        rows.extend(remote_rows);
    }

    // 已装版本列表
    if installed.is_empty() {
        rows.push(MenuItem::with_id(app, "dsh-none", "（无已装版本，请先安装 / 修复）", false, None::<&str>)?);
    } else {
        for (i, v) in installed.iter().enumerate() {
            let tag = v["tag"].as_str().unwrap_or("").to_string();
            let ver = v["version"].as_str().unwrap_or("").to_string();
            let is_active = v["active"].as_bool().unwrap_or(false) || tag == active_tag;
            let label = if is_active {
                format!("{ver}  ✓（当前）")
            } else {
                format!("{ver}")
            };
            // 当前版本不可切换；其他版本可切换
            rows.push(MenuItem::with_id(
                app,
                format!("dsh-switch-{i}"),
                label,
                !is_active,
                None::<&str>,
            )?);
        }
    }

    let refs: Vec<&dyn tauri::menu::IsMenuItem<R>> = rows
        .iter()
        .map(|m| m as &dyn tauri::menu::IsMenuItem<R>)
        .collect();
    Submenu::with_id_and_items(app, "dsh", "dsh 版本", true, &refs)
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
        // 复制连接 token（管理页「连接」时需要输入）
        rows.push(MenuItem::with_id(
            app,
            "bridge-copy-token",
            "📋 复制连接 token".to_string(),
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

/// 构建「同步 / 推荐插件」子菜单：待装/待更新推荐各一条 + 立即同步。
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

    // 待装/待更新清单：优先用缓存的服务端配置（离线也可显示）。
    // 判断口径 = 当前 profile 已装 + registry 最新版本（已装旧版 → 提示更新）
    let installed_with_ver = crate::sync::installed_plugins_current_profile_with_versions(app, &cfg);
    let state = crate::sync::load_state(app, &cfg);
    let pending_entries: Vec<serde_json::Value> = state
        .cached_config
        .as_ref()
        .map(|c| {
            crate::sync::pending_with_updates(
                &c.plugins,
                &installed_with_ver,
                &state.plugin_latest_versions,
            )
        })
        .unwrap_or_default();

    let mut status_items: Vec<MenuItem<R>> = Vec::new();
    let mut install_items: Vec<MenuItem<R>> = Vec::new();

    // 已装但未完成配置的插件提示（如 dsh-matrix-agent 未配 accessToken）
    let disabled_plugins = crate::sync::disabled_installed_plugins(app, &cfg);
    for d in &disabled_plugins {
        status_items.push(MenuItem::with_id(
            app,
            format!("sync-disabled-{}", d.replace(['/', '@'], "_")),
            format!("⚙️ {d} 已装待配置（设置页配置参数后生效）"),
            false,
            None::<&str>,
        )?);
    }

    if pending_entries.is_empty() {
        status_items.push(MenuItem::with_id(app, "sync-uptodate", "已是最新（无待装/待更新推荐）", false, None::<&str>)?);
    } else {
        for (i, entry) in pending_entries.iter().enumerate() {
            let name = entry["name"].as_str().unwrap_or("").to_string();
            let action = entry["action"].as_str().unwrap_or("install");
            let label = if action == "update" {
                let installed_v = entry["installed"].as_str().unwrap_or("");
                let latest_v = entry["latest"].as_str().unwrap_or("");
                format!("更新 {name}（{installed_v} → {latest_v}）")
            } else {
                format!("安装 {name}")
            };
            install_items.push(MenuItem::with_id(
                app,
                format!("sync-install-{i}"),
                label,
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
    let installed_with_ver = crate::sync::installed_plugins_current_profile_with_versions(app, &cfg);
    let state = crate::sync::load_state(app, &cfg);
    state
        .cached_config
        .as_ref()
        .and_then(|c| {
            crate::sync::pending_with_updates(&c.plugins, &installed_with_ver, &state.plugin_latest_versions)
                .get(index)
                .and_then(|p| p["name"].as_str().map(|s| s.to_string()))
        })
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
            match crate::console::open_console(app) {
                Ok(()) => {}
                Err(e) => {
                    log::error!("无法打开进度窗口：{e}");
                    notify(app, "无法打开进度窗口", &e);
                }
            }
        }
        "bridge-copy-token" => {
            let tok = crate::admin_bridge::current_token();
            if tok.is_empty() {
                notify(app, "复制 token", "管理能力未开启或未配置 token");
            } else {
                match copy_to_clipboard(&tok) {
                    Ok(()) => {
                        notify(app, "复制成功", "连接 token 已复制到剪贴板");
                        crate::ops::start_op(app, "bridge-copy", "复制 token", &[]);
                        crate::ops::finish_op(app, "连接 token 已复制到剪贴板");
                    }
                    Err(e) => notify(app, "复制失败", &e),
                }
            }
        }
        "dsh-check-update" => {
            let h = app.clone();
            tauri::async_runtime::spawn(async move {
                crate::ops::start_op(&h, "dsh-update", "检查 dsh 更新", &["检查更新"]);
                crate::ops::mark_step_running(&h, 0);
                crate::ops::update_step(&h, "查询远程版本…");
                let (current, latest, has_update) = crate::dsh_versions::check_update(&h).await;
                // latest 是 tag 名，显示时用规范化版本号
                let latest_display = latest
                    .as_deref()
                    .map(crate::dsh_versions::normalize_tag_version)
                    .unwrap_or_default();
                let msg = match (&latest, has_update) {
                    (Some(_), true) => format!(
                        "当前 {current}，发现新版本 {latest_display}\n已在本子菜单出现「📥 安装」项，点击即可下载"
                    ),
                    (Some(_), false) => format!("当前 {current}，已是最新（{latest_display}）"),
                    (None, _) => format!("当前 {current}，远程版本查询失败（网络受限？）"),
                };
                crate::ops::finish_op(&h, &msg);
                notify(&h, "dsh 版本检查", &msg);
                refresh_sync_menu(&h);
            });
        }
        id if id.starts_with("dsh-switch-") => {
            let idx = id
                .strip_prefix("dsh-switch-")
                .and_then(|s| s.parse::<usize>().ok());
            if let Some(idx) = idx {
                let installed = crate::dsh_versions::list_installed(app);
                if let Some(v) = installed.get(idx) {
                    let tag = v["tag"].as_str().unwrap_or("").to_string();
                    let h = app.clone();
                    tauri::async_runtime::spawn(async move {
                        crate::ops::start_op(&h, "dsh-switch", "切换 dsh 版本", &["停止 Harness", "替换版本", "重启 Harness"]);
                        crate::ops::mark_step_running(&h, 0);
                        crate::ops::update_step(&h, &format!("切换到 {tag}…"));
                        match crate::dsh_versions::switch_version(&h, &tag).await {
                            Ok((old, new)) => {
                                let msg = format!("dsh 版本切换完成：{old} -> {new}");
                                crate::ops::finish_op(&h, &msg);
                                notify(&h, "dsh 版本已切换", &msg);
                                refresh_sync_menu(&h);
                            }
                            Err(e) => {
                                crate::ops::fail_op(&h, &e);
                                notify(&h, "dsh 切换失败", &e);
                                refresh_sync_menu(&h);
                            }
                        }
                    });
                }
            }
        }
        id if id.starts_with("dsh-install-") => {
            let idx = id
                .strip_prefix("dsh-install-")
                .and_then(|s| s.parse::<usize>().ok());
            if let Some(idx) = idx {
                let remote = crate::dsh_versions::installable_remote_releases(app);
                if let Some(r) = remote.get(idx) {
                    let tag = r["tag"].as_str().unwrap_or("").to_string();
                    let version = r["version"].as_str().unwrap_or("").to_string();
                    let h = app.clone();
                    tauri::async_runtime::spawn(async move {
                        crate::ops::start_op(&h, "dsh-install", "安装 dsh 版本", &["下载", "安装"]);
                        crate::ops::mark_step_running(&h, 0);
                        crate::ops::update_step(&h, &format!("下载 {version}…"));
                        // 带进度回调 → 进度窗口
                        let h2 = h.clone();
                        let version_cb = version.clone();
                        let result = crate::dsh_versions::install_version(
                            &h,
                            &tag,
                            Some(&move |downloaded, total| {
                                let pct = if total > 0 {
                                    (downloaded as f64 / total as f64 * 100.0).round() as u32
                                } else {
                                    0
                                };
                                crate::ops::update_step(&h2, &format!("下载 {version_cb} {pct}%"));
                            }),
                        )
                        .await;
                        match result {
                            Ok(()) => {
                                let msg = format!("dsh {version} 已安装，可在本子菜单切换");
                                crate::ops::finish_op(&h, &msg);
                                notify(&h, "dsh 版本已安装", &msg);
                                refresh_sync_menu(&h);
                            }
                            Err(e) => {
                                crate::ops::fail_op(&h, &e);
                                notify(&h, "dsh 安装失败", &e);
                                refresh_sync_menu(&h);
                            }
                        }
                    });
                }
            }
        }
        "bridge-start" => {
            let h = app.clone();
            tauri::async_runtime::spawn(async move {
                crate::ops::start_op(&h, "bridge", "开启管理能力", &[]);
                match crate::admin_bridge::start(&h) {
                    Ok(port) => {
                        let _ = set_bridge_enabled(&h, true);
                        let tok = crate::admin_bridge::current_token();
                        let detail = if tok.is_empty() {
                            format!("本地 API：http://127.0.0.1:{port}")
                        } else {
                            format!("本地 API：http://127.0.0.1:{port}\n连接 token：{tok}\n（管理页连接时需输入；可在「管理能力」菜单复制 token）")
                        };
                        crate::ops::finish_op(&h, &detail);
                        notify(&h, "管理能力已开启", &detail);
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
                // 手动「立即同步」：强制刷新版本检查（忽略缓存，总是拿到 registry 最新）
                let outcome = crate::sync::sync_once(&h, &cfg, None, true).await;
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
                                // 安装后立即同步一次（刷新状态 + 上报服务端；用缓存 TTL，不强制）
                                let cfg = load_cached();
                                let _ = crate::sync::sync_once(&h, &cfg, None, false).await;
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
        id if id.starts_with("npm-preset-") => {
            if let Some(i) = id.strip_prefix("npm-preset-").and_then(|s| s.parse::<usize>().ok()) {
                if let Some((_, url)) = NPM_REGISTRY_PRESETS.get(i) {
                    apply_accel(app, "npm", url);
                }
            }
        }
        id if id.starts_with("gh-preset-") => {
            if let Some(i) = id.strip_prefix("gh-preset-").and_then(|s| s.parse::<usize>().ok()) {
                if let Some((_, url)) = GH_MIRROR_PRESETS.get(i) {
                    apply_accel(app, "gh", url);
                }
            }
        }
        "accel-speedtest" => {
            notify(app, "测速", "正在探测各加速源延迟，请稍候…");
            let h = app.clone();
            tauri::async_runtime::spawn(async move {
                crate::ops::start_op(&h, "speedtest", "加速源测速", &["探测 npm 源", "探测 GitHub 中转"]);
                crate::ops::mark_step_running(&h, 0);
                crate::ops::update_step(&h, "探测 npm 源…");
                let cfg = load_cached();
                let npm = crate::speedtest::speedtest_npm(&cfg).await;
                crate::ops::mark_step_running(&h, 1);
                crate::ops::update_step(&h, "探测 GitHub 中转…");
                let gh = crate::speedtest::speedtest_gh(&cfg).await;

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
                // 结果可能较长，截断到通知上限（按字符边界，防中文切片 panic）
                let msg = if msg.len() > 900 {
                    let cut = crate::config::truncate_utf8(&msg, 900);
                    format!("{cut}…")
                } else {
                    msg
                };
                // 缓存测速结果（托盘菜单显示各源速度）
                let mut all = npm.clone();
                all.extend(gh.clone());
                crate::speedtest::set_last_results(all);
                crate::ops::finish_op(&h, "测速完成（菜单「加速设置」可见各源速度）");
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
            QUIT_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
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

/// 复制文本到系统剪贴板（arboard，轻量跨平台）。
fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| format!("剪贴板不可用：{e}"))?;
    clipboard
        .set_text(text.to_string())
        .map_err(|e| format!("写入剪贴板失败：{e}"))
}
