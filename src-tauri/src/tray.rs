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
                    // ⚠️ 必须用带 token 的 URL：dsh 0.1.2+ 缺 ?token= 会 401
                    if let Some(url) = crate::workflow::current_access_url() {
                        let _ = tray.app_handle().opener().open_url(url, None::<&str>);
                    }
                }
            }
        })
        .build(app)?;

    // 初始 tooltip：版本 + 状态（悬停提示），状态变化由 refresh_sync_menu → update_tray_tooltip 刷新
    update_tray_tooltip(app);

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
    update_tray_tooltip(app);
}

/// 刷新系统托盘图标 tooltip：鼠标悬停图标时显示「launcher 版本 + 数字分身 / 管理能力状态」。
/// 复用 refresh_sync_menu 的调用点（同步/启停/bridge 等操作后都会走到），
/// 用 tray_by_id 原地更新，无需额外存储句柄。
pub fn update_tray_tooltip<R: Runtime>(app: &AppHandle<R>) {
    let mut parts = vec![format!("DeepSeek Harness Launcher v{}", env!("CARGO_PKG_VERSION"))];

    // 数字分身（matrix profile 进程）运行态
    parts.push(if crate::workflow::is_running() {
        "数字分身：运行中".to_string()
    } else {
        "数字分身：停止".to_string()
    });

    // 数字分身账号是否已配置（区分「没配 / 配了」）
    let cfg = load_cached();
    match crate::matrix_setup::status(app, &cfg) {
        crate::matrix_setup::MatrixStatus::Configured => parts.push("账号：已配置".to_string()),
        crate::matrix_setup::MatrixStatus::NotInstalled => parts.push("账号：未安装".to_string()),
        crate::matrix_setup::MatrixStatus::Unconfigured { .. } => parts.push("账号：未配置".to_string()),
    }

    // 管理能力（bridge）运行态
    parts.push(if crate::admin_bridge::is_running() {
        format!("管理能力：开启（端口 {}）", crate::admin_bridge::port().map(|p| p.to_string()).unwrap_or_else(|| "?".into()))
    } else {
        "管理能力：关闭".to_string()
    });

    let tip = parts.join("  ·  ");
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        if let Err(e) = tray.set_tooltip(Some(tip)) {
            log::warn!("更新托盘 tooltip 失败：{e}");
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
    // 已装插件 子菜单（动态：一键卸载）
    let plugins_submenu = build_plugins_submenu(app)?;
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
    // 首次运行（dsh 未安装）：顶部提示 + 「首次使用向导」入口
    // （小白关掉欢迎窗口后仍能从托盘找回引导）
    if crate::first_run::is_first_run(app) {
        owned.push(MenuItem::with_id(app, "fr-warn", "👋 首次使用：请点下方「首次使用向导」", false, None::<&str>)?);
        owned.push(MenuItem::with_id(app, "fr-open", "🚀 首次使用向导（安装）", true, None::<&str>)?);
    }

    // 数字分身配置状态提示（未配置时顶部提示 + 入口；已配置隐藏提示）
    let cfg0 = load_cached();
    if crate::matrix_setup::matrix_agent_installed(app, &cfg0) {
        match crate::matrix_setup::status(app, &cfg0) {
            crate::matrix_setup::MatrixStatus::Unconfigured { .. } => {
                owned.push(MenuItem::with_id(app, "ms-warn", "⚠️ 数字分身待配置（点击下方「配置数字分身」）", false, None::<&str>)?);
                owned.push(MenuItem::with_id(app, "ms-open", "🛠 配置数字分身", true, None::<&str>)?);
            }
            crate::matrix_setup::MatrixStatus::NotInstalled => {}
            crate::matrix_setup::MatrixStatus::Configured => {
                owned.push(MenuItem::with_id(app, "ms-open", "🛠 数字分身设置", true, None::<&str>)?);
            }
        }
    }

    // HiMarket 登录状态：SSO 一键登录（developer token 7 天过期后重新登录用）。
    // 与 dsh-himarket 插件共用 settings.yaml 的 himarket namespace。
    match crate::matrix_setup::himarket_token_state(app, &cfg0) {
        crate::matrix_setup::HimarketTokenState::LoggedIn { username, display_name } => {
            // 已登录：菜单项直接显示「谁已登录」（姓名优先，回落账号）
            let who = if display_name.trim().is_empty() { username } else { display_name };
            let label = if who.trim().is_empty() {
                "🔑 HiMarket 重新登录".to_string()
            } else {
                format!("🔑 HiMarket 已登录：{who}")
            };
            owned.push(MenuItem::with_id(app, "hm-login", label, true, None::<&str>)?);
        }
        crate::matrix_setup::HimarketTokenState::NotLoggedIn => {
            owned.push(MenuItem::with_id(app, "hm-warn", "⚠️ HiMarket 未登录（点击下方「一键登录」）", false, None::<&str>)?);
            owned.push(MenuItem::with_id(app, "hm-login", "🔑 HiMarket 一键登录", true, None::<&str>)?);
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
    items.push(&plugins_submenu);
    items.push(&url_submenu);
    items.push(&accel_submenu);
    // 高级组
    items.push(&dsh_submenu);
    items.push(&bridge_submenu);

    // 收尾：查看日志 / 收集日志（排障回传） / 重装内网证书 / 一键重置（清空 DSH 数据） / 退出
    let log_item = MenuItem::with_id(app, "log", "查看日志", true, None::<&str>)?;
    let logpack_item = MenuItem::with_id(app, "logpack", "📦 收集日志（发给管理员排障）", true, None::<&str>)?;
    let cert_item = MenuItem::with_id(app, "cert-reinstall", "🔐 重装内网证书", true, None::<&str>)?;
    let reset_item = MenuItem::with_id(app, "reset", "🗑 重置（清空数字分身数据）", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    // launcher 自身版本 + 更新入口（启动时自检的结果显示在这里）
    let launcher_ver_item = MenuItem::with_id(
        app,
        "launcher-version",
        crate::self_update::last_check()
            .map(|(o, _)| o.menu_label())
            .unwrap_or_else(|| format!("launcher v{}", env!("CARGO_PKG_VERSION"))),
        false,
        None::<&str>,
    )?;
    let launcher_check_item = MenuItem::with_id(app, "launcher-check-update", "🔄 检查 launcher 更新", true, None::<&str>)?;
    items.push(&launcher_ver_item);
    items.push(&launcher_check_item);
    items.push(&log_item);
    items.push(&logpack_item);
    items.push(&cert_item);
    items.push(&reset_item);
    items.push(&quit_item);

    let menu = Menu::with_items(app, &items)?;
    Ok(menu)
}

/// 构建「dsh 版本」子菜单：当前版本 + 检查更新 + 版本通道 + 已装版本切换 + 远程可安装版本。
fn build_dsh_version_submenu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Submenu<R>> {
    // 用 IsMenuItem 而非 MenuItem：需要混排普通项与勾选项（CheckMenuItem）。
    let mut rows: Vec<Box<dyn tauri::menu::IsMenuItem<R>>> = Vec::new();
    let active = crate::dsh_versions::active_version(app);
    let installed = crate::dsh_versions::list_installed(app);
    let active_tag = crate::dsh_versions::active_tag(app);

    // 状态行：当前版本
    let status_text = if active.is_empty() {
        "dsh 未安装".to_string()
    } else {
        format!("当前 dsh：{active}")
    };
    rows.push(Box::new(MenuItem::with_id(app, "dsh-status", status_text, false, None::<&str>)?));
    // 检查更新（点击触发异步查询）
    rows.push(Box::new(MenuItem::with_id(app, "dsh-check-update", "🔍 检查更新", true, None::<&str>)?));

    // 版本通道开关：默认只列 RC/正式版；勾选后列出 Alpha 等预发布。
    // 安全默认——DSH 发版快，Alpha 不该被无意间装到（见 config::DshChannel 文档）。
    {
        use tauri::menu::CheckMenuItem;
        let cfg = crate::config::load_cached();
        let ch = crate::config::DshChannel::parse(cfg.dsh_channel.as_deref());
        let show_alpha = ch == crate::config::DshChannel::Alpha;
        rows.push(Box::new(MenuItem::with_id(
            app,
            "dsh-channel-sep",
            "─ 版本通道 ─",
            false,
            None::<&str>,
        )?));
        rows.push(Box::new(CheckMenuItem::with_id(
            app,
            "dsh-channel-alpha",
            "显示 Alpha 版本（默认关闭）",
            true,
            show_alpha,
            None::<&str>,
        )?));
    }

    // 远程可安装版本（「检查更新」后缓存；点击即下载安装；已装的自动过滤）
    let remote = crate::dsh_versions::installable_remote_releases(app);
    let remote_rows: Vec<MenuItem<R>> = remote
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let version = r["version"].as_str().unwrap_or("").to_string();
            let prerelease = r["prerelease"].as_bool().unwrap_or(false);
            // 预发布醒目标注，降低误装风险
            let label = if prerelease {
                format!("⚠ 安装 {version}（预发布）")
            } else {
                format!("📥 安装 {version}")
            };
            MenuItem::with_id(app, format!("dsh-install-{i}"), label, true, None::<&str>)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !remote_rows.is_empty() {
        rows.push(Box::new(MenuItem::with_id(
            app,
            "dsh-remote-sep",
            "─ 远程可用版本 ─",
            false,
            None::<&str>,
        )?));
        rows.extend(remote_rows.into_iter().map(|m| Box::new(m) as Box<dyn tauri::menu::IsMenuItem<R>>));
    }

    // 已装版本列表
    if installed.is_empty() {
        rows.push(Box::new(MenuItem::with_id(
            app,
            "dsh-none",
            "（无已装版本，请先安装 / 修复）",
            false,
            None::<&str>,
        )?));
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
            rows.push(Box::new(MenuItem::with_id(
                app,
                format!("dsh-switch-{i}"),
                label,
                !is_active,
                None::<&str>,
            )?));
        }
    }

    let refs: Vec<&dyn tauri::menu::IsMenuItem<R>> = rows
        .iter()
        .map(|m| m.as_ref() as &dyn tauri::menu::IsMenuItem<R>)
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
    // 判断口径 = 当前 profile 已装 + registry 最新版本（已装旧版 → 提示更新）。
    // 清单按当前 profile 精确取（profilePlugins[当前] 优先，回落全局 plugins）。
    // 与「一键全部」共用 pending_entries()，避免两处口径漂移。
    let pending_entries: Vec<serde_json::Value> = pending_entries(app);
    // 服务端缓存状态（下架清单等）仍需要
    let state = crate::sync::load_state(app, &cfg);

    let mut status_items: Vec<MenuItem<R>> = Vec::new();
    let mut install_items: Vec<MenuItem<R>> = Vec::new();
    let mut remove_items: Vec<MenuItem<R>> = Vec::new();
    let mut broken_items: Vec<MenuItem<R>> = Vec::new();

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

    // ⚠️ 影响启动：被 DSH 兜底禁用（disabled: true）的插件 —— 不管谁装的，一律展示，
    // 让用户决定是否一键清理（场景②）。判定数据源 = disabled:true（DSH 客观判定）。
    let broken_plugins = crate::sync::startup_broken_plugins(app, &cfg);
    for (i, b) in broken_plugins.iter().enumerate() {
        broken_items.push(MenuItem::with_id(
            app,
            format!("sync-broken-{i}"),
            format!("⚠️ {b} 影响启动（已被禁用，可清理）"),
            true,
            None::<&str>,
        )?);
    }

    // 🗑 建议卸载：管理员已从服务端清单下架（曾推荐过、本次已移除）—— 场景①。
    // 只针对曾出现在服务端推荐清单里的插件，同事自己装的绝不进入此列表。
    let removed_plugins = state.last_removed.clone();
    for (i, r) in removed_plugins.iter().enumerate() {
        remove_items.push(MenuItem::with_id(
            app,
            format!("sync-remove-{i}"),
            format!("🗑 {r}（管理员已下架，建议卸载）"),
            true,
            None::<&str>,
        )?);
    }

    if pending_entries.is_empty() {
        status_items.push(MenuItem::with_id(app, "sync-uptodate", "已是最新（无待装/待更新推荐）", false, None::<&str>)?);
    } else {
        // ⚡ 批量入口：待处理 >1 个时提供「一键全部」，不必逐个点。
        // 排在各单项之前（最省事的入口放最上）。
        if pending_entries.len() > 1 {
            let upd = pending_entries
                .iter()
                .filter(|e| e["action"].as_str() == Some("update"))
                .count();
            let ins = pending_entries.len() - upd;
            let label = batch_menu_label(pending_entries.len(), upd, ins);
            install_items.push(MenuItem::with_id(app, "sync-install-all", label, true, None::<&str>)?);
        }
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
    // 分隔符菜单项需在 items 借用集合之外持有（owned），否则临时值被提前 drop。
    let sep_broken = if broken_items.is_empty() {
        None
    } else {
        Some(MenuItem::with_id(app, "sync-sep-broken", "─ ⚠️ 影响启动 ─", false, None::<&str>)?)
    };
    let sep_remove = if remove_items.is_empty() {
        None
    } else {
        Some(MenuItem::with_id(app, "sync-sep-remove", "─ 🗑 建议卸载 ─", false, None::<&str>)?)
    };
    // 影响启动 → 排最前（最紧急）
    if let Some(sep) = &sep_broken {
        items.push(sep);
        items.extend(broken_items.iter().map(|m| m as &dyn tauri::menu::IsMenuItem<R>));
    }
    // 建议卸载
    if let Some(sep) = &sep_remove {
        items.push(sep);
        items.extend(remove_items.iter().map(|m| m as &dyn tauri::menu::IsMenuItem<R>));
    }
    items.extend(status_items.iter().map(|m| m as &dyn tauri::menu::IsMenuItem<R>));
    items.extend(install_items.iter().map(|m| m as &dyn tauri::menu::IsMenuItem<R>));
    // 立即同步（手动触发）
    let refresh = MenuItem::with_id(app, "sync-now", "立即同步", true, None::<&str>)?;
    items.push(&refresh);

    Submenu::with_id_and_items(app, "sync", "同步 / 推荐插件", true, &items)
}

/// 构建「已装插件」子菜单：列出当前 profile 已装插件，点击二次确认后一键卸载。
///
/// 数据源 = 当前 profile 的 package.json `dependencies`（node_modules 里真实存在）。
/// 这正是「用户能看到报错、知道是哪个插件错了，直接来菜单点它卸载」的入口。
fn build_plugins_submenu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Submenu<R>> {
    let cfg = load_cached();
    let current_profile = resolve_profile(&cfg);
    let installed = crate::sync::installed_plugins_current_profile_with_versions(app, &cfg);

    // 标题行：显示当前 profile 及已装数量
    let header = format!("当前 profile：{current_profile}（已装 {} 个）", installed.len());
    let header_item = MenuItem::with_id(app, "plugins-header", header, false, None::<&str>)?;

    let mut plugin_items: Vec<MenuItem<R>> = Vec::new();
    if installed.is_empty() {
        plugin_items.push(MenuItem::with_id(app, "plugins-none", "（未安装任何插件）", false, None::<&str>)?);
    } else {
        // 按名字排序，稳定可预测
        let mut sorted: Vec<(&String, &String)> = installed.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(b.0));
        for (i, (name, version)) in sorted.iter().enumerate() {
            let label = if version.is_empty() {
                format!("🗑 {name}")
            } else {
                format!("🗑 {name}  v{version}")
            };
            plugin_items.push(MenuItem::with_id(
                app,
                format!("plugin-uninstall-{i}"),
                label,
                true,
                None::<&str>,
            )?);
        }
    }

    let mut items: Vec<&dyn tauri::menu::IsMenuItem<R>> = Vec::new();
    items.push(&header_item);
    items.extend(plugin_items.iter().map(|m| m as &dyn tauri::menu::IsMenuItem<R>));

    Submenu::with_id_and_items(app, "plugins", "已装插件", true, &items)
}

/// 供菜单点击时取「第 i 个已装插件名」（与 build_plugins_submenu 的排序一致）。
fn installed_plugin_at<R: Runtime>(app: &AppHandle<R>, index: usize) -> Option<String> {
    let cfg = load_cached();
    let installed = crate::sync::installed_plugins_current_profile_with_versions(app, &cfg);
    let mut sorted: Vec<(&String, &String)> = installed.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    sorted.get(index).map(|(name, _)| (*name).clone())
}

/// 取「第 i 个待装/待更新插件」的完整条目（含 name/installed/latest/action），
/// 与 `build_sync_submenu` 的索引一致。
///
/// 更新向导需要展示「0.1.7 → 0.1.8」这类信息，仅名字不够，故返回整个 JSON 条目。
fn pending_entry_at<R: Runtime>(app: &AppHandle<R>, index: usize) -> Option<serde_json::Value> {
    pending_entries(app).get(index).cloned()
}

/// 当前 profile 的完整待装/待更新清单（与 `build_sync_submenu` 同一口径）。
///
/// 单独抽出是为了让「一键全部」与逐个入口**共用同一份判定**：两处若各自
/// 计算，一旦口径漂移（如批量入口漏了 profile 过滤），用户会看到菜单列出 3 个、
/// 实际只处理 2 个，且难以察觉。
fn pending_entries<R: Runtime>(app: &AppHandle<R>) -> Vec<serde_json::Value> {
    let cfg = load_cached();
    let current_profile = resolve_profile(&cfg);
    let installed_with_ver = crate::sync::installed_plugins_current_profile_with_versions(app, &cfg);
    let state = crate::sync::load_state(app, &cfg);
    state
        .cached_config
        .as_ref()
        .map(|c| {
            let cur = crate::sync::plugins_for_profile(c, &current_profile);
            crate::sync::pending_with_updates(&cur, &installed_with_ver, &state.plugin_latest_versions)
        })
        .unwrap_or_default()
}

/// 供菜单点击时取「第 i 个建议卸载插件名」（与 build_sync_submenu 的索引一致）。
fn removable_plugin_at<R: Runtime>(app: &AppHandle<R>, index: usize) -> Option<String> {
    let cfg = load_cached();
    let state = crate::sync::load_state(app, &cfg);
    state.last_removed.get(index).cloned()
}

/// 供菜单点击时取「第 i 个影响启动插件名」（与 build_sync_submenu 的索引一致）。
fn broken_plugin_at<R: Runtime>(app: &AppHandle<R>, index: usize) -> Option<String> {
    let cfg = load_cached();
    crate::sync::startup_broken_plugins(app, &cfg).get(index).cloned()
}

/// 卸载前的二次确认框（原生 MessageBox，`确定`/`取消`）。
///
/// 返回 true = 用户确认卸载。Windows 用 `MessageBoxW`（windows-sys 已有依赖，
/// 无需新增 crate）；非 Windows 平台无确认框，直接放行（桌面端仅面向 Windows）。
fn confirm_uninstall(name: &str, reason: &str) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            MessageBoxW, MB_ICONWARNING, MB_OKCANCEL, IDOK,
        };
        let title: Vec<u16> = "确认卸载插件".encode_utf16().chain(std::iter::once(0)).collect();
        let body: Vec<u16> = format!(
            "{reason}\n\n确定要卸载插件「{name}」吗？\n\n卸载后可随时重新安装。"
        )
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
        let ret = unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                body.as_ptr(),
                title.as_ptr(),
                MB_OKCANCEL | MB_ICONWARNING,
            )
        };
        ret == IDOK
    }
    #[cfg(not(windows))]
    {
        let _ = (name, reason);
        true
    }
}

/// 插件安装/更新后自动重启 Harness，使新版本生效（Q3：用户不必再手点「停止+启动」）。
///
/// 语义与用户诉求一致：
/// - Harness **本来就在运行** → 停止并重启（新插件版本需重载才生效）；
/// - Harness **本来没运行** → 不动它（不替用户决定要启动）；
/// - 重启失败 → 返回 false 并写日志，由调用方在结果里如实体现（不谎报成功）。
///
/// `step_index` = 本次操作里「重启」那一步的下标。单个更新是 1；
/// 批量更新时步骤列表长度 = 插件数 + 1（重启），故不能写死。
///
/// 返回是否真的完成了「重启」。
async fn auto_restart_harness<R: Runtime>(app: &AppHandle<R>, plugin: &str, step_index: usize) -> bool {
    if !crate::workflow::is_running() {
        crate::ops::append_log(app, "Harness 未在运行，跳过重启（下次启动即加载新版本）");
        return false;
    }
    // 第 1 步：重启 Harness（含停止 + 启动 + 等端口就绪）
    crate::ops::mark_step_running(app, step_index);
    crate::ops::update_step(app, "正在重启 Harness 使新版本生效…");
    crate::ops::append_log(app, "更新后需重载插件：正在重启 Harness…");

    let profile = crate::workflow::current_profile().unwrap_or_else(|| resolve_profile(&load_cached()));
    crate::workflow::stop();
    match crate::workflow::launch_with_profile(app, &profile) {
        Ok(port) => {
            let url = crate::workflow::access_url(port);
            crate::ops::append_log(app, &format!("✓ Harness 已重启，访问 {url}"));
            crate::ops::update_step(app, &format!("✓ {plugin} 已生效"));
            true
        }
        Err(e) => {
            // 重启失败不掩盖：明确告知 + 引导用户手动启动（不谎报「已就绪」）
            log::error!("插件更新后重启 Harness 失败：{e}");
            crate::ops::append_log(app, &format!("✗ 自动重启失败：{e}"));
            crate::ops::mark_step_failed(app, step_index);
            notify(
                app,
                "插件已更新，但 Harness 重启失败",
                &format!("{plugin} 已安装。请手动点托盘「启动 Harness」。\n\n原因：{e}"),
            );
            false
        }
    }
}

/// 「一键全部」菜单项的文案（纯函数，便于测试）。
///
/// 更新与安装要分开计数：只说「全部处理（3 个）」用户不知道有几个是升级。
fn batch_menu_label(total: usize, updates: usize, installs: usize) -> String {
    match (updates, installs) {
        (_, 0) => format!("⬆️ 全部更新（{updates} 个）"),
        (0, _) => format!("⬇️ 全部安装（{installs} 个）"),
        _ => format!("⚡ 全部处理（{total} 个：更新 {updates} / 安装 {installs}）"),
    }
}

/// 批量向导里「单个插件」那一步的文案（纯函数，便于测试）。
///
/// 更新与安装必须能一眼区分：只写插件名，用户看不出这次是升级还是首装。
fn batch_step_label(name: &str, installed_v: &str, latest_v: &str, is_update: bool) -> String {
    if is_update {
        format!("更新 {name}（{installed_v} → {latest_v}）")
    } else {
        format!("安装 {name}（{latest_v}）")
    }
}

/// 批量处理的最终结论（纯函数，便于测试）。
///
/// 三条硬要求：
/// 1. **有失败就绝不说「全部完成」** —— 否则用户以为全好了，实际有几个没装上；
/// 2. 失败项**逐条列出含真因**，不能只给个数（用户要据此决定下一步）；
/// 3. 只有真的重启了才说「已自动重启生效」，不谎报。
fn batch_summary(total: usize, ok: usize, failed: &[(String, String)], restarted: bool) -> String {
    let mut s = if failed.is_empty() {
        format!("{total} 个插件全部处理完成")
    } else {
        format!("完成 {ok} 个，失败 {} 个", failed.len())
    };
    if restarted {
        s.push_str("，Harness 已自动重启生效");
    }
    for (name, err) in failed {
        s.push_str(&format!("\n✗ {name}：{err}"));
    }
    s
}

fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, event: tauri::menu::MenuEvent) {
    match event.id().as_ref() {
        "fr-open" => {
            let h = app.clone();
            tauri::async_runtime::spawn(async move {
                match crate::first_run::open_window(&h) {
                    Ok(()) => {}
                    Err(e) => notify(&h, "无法打开首次使用向导", &e),
                }
            });
        }
        "ms-open" => {
            let h = app.clone();
            tauri::async_runtime::spawn(async move {
                match crate::matrix_setup::open_window(&h) {
                    Ok(()) => {}
                    Err(e) => notify(&h, "无法打开配置向导", &e),
                }
            });
        }
        "hm-login" => {
            // HiMarket 一键登录（SSO）：走系统浏览器 + 本地回调，成功后写
            // settings.yaml 的 himarket.token。不碰数字分身进程，无需重启。
            if crate::ops::has_running() {
                notify(app, "HiMarket 登录", "已有操作进行中，请稍候");
                return;
            }
            let h = app.clone();
            tauri::async_runtime::spawn(async move {
                crate::ops::start_op(&h, "himarket-login", "HiMarket 一键登录", &["浏览器授权", "登录 HiMarket", "写入配置"]);
                crate::ops::mark_step_running(&h, 0);
                crate::ops::update_step(&h, "等待浏览器授权…");
                crate::ops::append_log(&h, "已打开浏览器，请在浏览器中完成公司 SSO 登录…");
                let r = crate::activation::run_himarket_login(&h);
                if r.ok {
                    crate::ops::finish_op(&h, &r.message);
                    notify(&h, "HiMarket 已登录", "岗位同步 / 安装技能现在可用");
                } else {
                    crate::ops::fail_op(&h, &r.message);
                    notify(&h, "HiMarket 登录失败", &r.message);
                }
                refresh_sync_menu(&h);
            });
        }
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
                        // 提示与打开都用带 token 的 URL（否则用户手点会 401）
                        let url = crate::workflow::access_url(port);
                        crate::ops::finish_op(&h, &format!("已启动，访问 {url}"));
                        notify(&h, "Harness 已启动", &format!("访问 {url}"));
                        refresh_sync_menu(&h);
                    }
                    Err(e) => {
                        crate::ops::fail_op(&h, &e);
                        // 失败时引导排障：提示可一键收集日志发给管理员（小白友好）
                        crate::ops::append_log(&h, "提示：可在托盘菜单点「📦 收集日志（发给管理员排障）」导出诊断包");
                        notify(
                            &h,
                            "启动失败",
                            &format!("{e}\n\n可在托盘菜单点「📦 收集日志」导出诊断包发给管理员"),
                        );
                        refresh_sync_menu(&h);
                    }
                }
            });
        }
        "open-page" => {
            // 以实际运行端口为准；未运行则尝试配置端口（可能尚未拉起，保持原行为）。
            // ⚠️ 运行中时必须带 token（dsh 0.1.2+ 缺 ?token= 会 401 打不开）。
            let url = match crate::workflow::current_access_url() {
                Some(u) => u,
                None => format!("http://127.0.0.1:{}", resolve_port(&load_cached())),
            };
            let _ = app.opener().open_url(url, None::<&str>);
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
                                let url = crate::workflow::access_url(port);
                                crate::ops::finish_op(&h, &format!("{name}：{url}"));
                                notify(&h, "Profile 已切换", &format!("{name}：{url}"));
                                refresh_sync_menu(&h);
                                // 切换后对齐插件清单：装新 profile 的插件、自动卸下架的旧预置
                                // （实现「选中哪个 profile 就该是哪个」，清单由服务端下发）
                                crate::install::apply_profile_plugins(&h).await;
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
        // 版本通道开关：切换 rc ⇄ alpha 并刷新菜单。
        // 打开 Alpha 时给明确提示（预发布版本可能不稳定）。
        "dsh-channel-alpha" => {
            let cfg = crate::config::load_cached();
            let cur = crate::config::DshChannel::parse(cfg.dsh_channel.as_deref());
            let next = if cur == crate::config::DshChannel::Alpha {
                crate::config::DshChannel::Rc
            } else {
                crate::config::DshChannel::Alpha
            };
            let next_str = match next {
                crate::config::DshChannel::Rc => "rc",
                crate::config::DshChannel::Alpha => "alpha",
            };
            let mut c = crate::config::load_cached();
            c.dsh_channel = Some(next_str.to_string());
            match crate::config::save_config(app, &c) {
                Ok(()) => {
                    let msg = match next {
                        crate::config::DshChannel::Rc => {
                            "已切回 RC 通道：只列出 RC / 正式版".to_string()
                        }
                        crate::config::DshChannel::Alpha => {
                            "已开启 Alpha 通道：将列出预发布版本\n⚠️ Alpha 可能不稳定，请谨慎安装".to_string()
                        }
                    };
                    notify(app, "dsh 版本通道", &msg);
                    // 重查远程版本（新通道下候选集变了）
                    let h = app.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = crate::dsh_versions::check_update(&h).await;
                        refresh_sync_menu(&h);
                    });
                }
                Err(e) => notify(app, "切换失败", &e),
            }
        }
        "launcher-check-update" => {
            // 手动检查 launcher 更新：先只检查并**如实报告结果**（有/无/失败三种都提示），
            // 再决定是否下载。绝不静默——「点了没反应」正是本轮要修的问题。
            let h = app.clone();
            tauri::async_runtime::spawn(async move {
                crate::ops::start_op(&h, "launcher-check", "检查 launcher 更新", &["查询服务端发布"]);
                crate::ops::mark_step_running(&h, 0);
                crate::ops::update_step(&h, "查询服务端版本…");
                let outcome = crate::self_update::check_only(&h).await;
                let msg = outcome.message();
                match &outcome {
                    crate::self_update::CheckOutcome::Failed { .. } => {
                        crate::ops::fail_op(&h, &msg);
                        notify(&h, "launcher 更新检查失败", &msg);
                    }
                    _ => {
                        crate::ops::finish_op(&h, &msg);
                        notify(&h, "launcher 更新检查", &msg);
                    }
                }
                refresh_sync_menu(&h);
                // 发现新版 → 继续走下载/替换（进度窗口会切成「更新 launcher」四步）
                if outcome.has_update() {
                    let _ = crate::self_update::check_and_update(&h).await;
                }
            });
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
                    let mut parts: Vec<String> = Vec::new();
                    if outcome.pending.is_empty() {
                        parts.push("已是最新，无待安装推荐插件".to_string());
                    } else {
                        parts.push(format!("待安装推荐：{}", outcome.pending.join(", ")));
                    }
                    if !outcome.removed.is_empty() {
                        parts.push(format!("管理员已下架：{}（可在菜单「建议卸载」清理）", outcome.removed.join(", ")));
                    }
                    let msg = parts.join("\n");
                    crate::ops::finish_op(&h, &msg);
                    notify(&h, "同步完成", &msg);
                    let _ = config;
                } else {
                    crate::ops::fail_op(&h, "无法连接中心服务端（已使用本地缓存）");
                    notify(&h, "同步失败", "无法连接中心服务端（已使用本地缓存）");
                }
            });
        }
        id if id == "sync-install-all" => {
            // ⚡ 一键全部：把待装/待更新清单**在一个向导里**逐个处理完。
            // 用户诉求原文：「显示多余 1 个插件需要更新，现在只能一个一个更新，
            // 建议有更新所有菜单」——逐点 N 次既费事，又每次都触发一次重启。
            let entries = pending_entries(app);
            if entries.is_empty() {
                notify(app, "无需处理", "当前没有待安装/待更新的推荐插件");
                return;
            }
            // 快照成纯数据（entry 是 serde_json::Value，可安全 move 进 async 块）
            let items: Vec<(String, String, String, String, bool)> = entries
                .iter()
                .map(|e| {
                    let action = e["action"].as_str().unwrap_or("install").to_string();
                    (
                        e["name"].as_str().unwrap_or("").to_string(),
                        action.clone(),
                        e["installed"].as_str().unwrap_or("").to_string(),
                        e["latest"].as_str().unwrap_or("").to_string(),
                        action == "update",
                    )
                })
                .collect();
            let total = items.len();
            let was_running = crate::workflow::is_running();
            let h = app.clone();
            tauri::async_runtime::spawn(async move {
                // 步骤列表：每个插件一步（带名字，用户能看到「正在更新哪个、哪些已完成」），
                // 末尾在 Harness 原本运行时追加「重启」一步。
                let mut step_labels: Vec<String> = items
                    .iter()
                    .map(|(name, _, installed_v, latest_v, is_update)| {
                        batch_step_label(name, installed_v, latest_v, *is_update)
                    })
                    .collect();
                if was_running {
                    step_labels.push("重启 Harness 使其生效".to_string());
                }
                let step_refs: Vec<&str> = step_labels.iter().map(|s| s.as_str()).collect();

                crate::ops::start_op(&h, "plugin-install-all", &format!("一键处理 {total} 个插件"), &step_refs);
                // ① 计划区：逐个列出「将要做什么」，点之前就看得见
                let mut details = vec![format!("共 {total} 个待处理（目标 profile：{}）", resolve_profile(&load_cached()))];
                for (name, _, installed_v, latest_v, is_update) in &items {
                    details.push(if *is_update {
                        format!("{name}：{installed_v} → {latest_v}")
                    } else {
                        format!("{name}：新安装（{latest_v}）")
                    });
                }
                if was_running {
                    details.push("全部完成后自动重启 Harness 使新版本生效（只重启一次）".to_string());
                }
                crate::ops::set_details(&h, &details);
                if let Err(e) = crate::console::open_console(&h) {
                    log::warn!("批量更新向导窗口打开失败（降级为通知）：{e}");
                }

                // ② 逐个处理：成功一个标一个 ✓，失败一个标一个 ✗，**不中断**后续
                let mut ok: Vec<String> = Vec::new();
                let mut failed: Vec<(String, String)> = Vec::new();
                for (i, (name, _, _installed_v, _latest_v, is_update)) in items.iter().enumerate() {
                    let verb = if *is_update { "更新" } else { "安装" };
                    crate::ops::mark_step_running(&h, i);
                    crate::ops::update_step(&h, &format!("正在{verb} {name}（{}/{total}）…", i + 1));
                    match crate::sync::install_plugin(&h, name).await {
                        Ok(()) => {
                            crate::ops::append_log(&h, &format!("✓ {name} 已{verb}"));
                            crate::ops::mark_step_done(&h, i);
                            ok.push(name.clone());
                        }
                        Err(e) => {
                            // 单个失败不阻断其余：用户要的是「一次点完」，不是「一错全停」。
                            // 失败如实标红 + 进结论，绝不混进成功里。
                            crate::ops::append_log(&h, &format!("✗ {name} {verb}失败：{e}"));
                            crate::ops::mark_step_failed(&h, i);
                            failed.push((name.clone(), e));
                        }
                    }
                }

                // ③ 收尾：只有真的装成了东西才重启（全失败就没必要打断用户）
                let restarted = if was_running && !ok.is_empty() {
                    auto_restart_harness(&h, &format!("{} 个插件", ok.len()), total).await
                } else {
                    if was_running {
                        crate::ops::append_log(&h, "没有任何插件成功，跳过重启");
                    } else {
                        crate::ops::append_log(&h, "Harness 未在运行，无需重启（下次启动即加载新版本）");
                    }
                    false
                };

                // 结论：成功 N / 失败 M，失败项逐条列出（含真因），不笼统说「已完成」
                let summary = batch_summary(total, ok.len(), &failed, restarted);
                if failed.is_empty() {
                    crate::ops::finish_op(&h, &summary);
                    notify(&h, "插件已全部处理", &summary);
                } else {
                    // 有失败 → 整体算失败（否则用户会以为全好了）
                    crate::ops::fail_op(&h, &summary);
                    notify(&h, "部分插件处理失败", &summary);
                }
                let cfg = load_cached();
                let _ = crate::sync::sync_once(&h, &cfg, None, false).await;
                refresh_sync_menu(&h);
            });
        }
        id if id.starts_with("sync-install-") => {
            let idx = id
                .strip_prefix("sync-install-")
                .and_then(|s| s.parse::<usize>().ok());
            if let Some(idx) = idx {
                if let Some(entry) = pending_entry_at(app, idx) {
                    let name = entry["name"].as_str().unwrap_or("").to_string();
                    let action = entry["action"].as_str().unwrap_or("install").to_string();
                    let installed_v = entry["installed"].as_str().unwrap_or("").to_string();
                    let latest_v = entry["latest"].as_str().unwrap_or("").to_string();
                    let is_update = action == "update";
                    // Harness 是否在运行，决定向导是否有「重启」这一步（如实展示，不摆空步骤）
                    let was_running = crate::workflow::is_running();
                    let h = app.clone();
                    let name_clone = name.clone();
                    tauri::async_runtime::spawn(async move {
                        // ── 更新向导（Q1/Q3）：不再黑盒执行 ──────────────────
                        // ① 先展示「将要更新什么」，② 逐步显示进度，③ 完成后自动重启
                        let title = if is_update { "更新插件" } else { "安装插件" };
                        let steps: Vec<&str> = if was_running {
                            vec!["安装插件", "重启 Harness 使其生效"]
                        } else {
                            vec!["安装插件"]
                        };
                        crate::ops::start_op(&h, "plugin-install", title, &steps);
                        // ① 让用户看到要更新的内容（名称/版本/来源）
                        let plan = if is_update {
                            format!("{name_clone}：{installed_v} → {latest_v}")
                        } else {
                            format!("{name_clone}：新安装（{latest_v}）")
                        };
                        let mut details = vec![plan.clone(), format!("目标 profile：{}", resolve_profile(&load_cached()))];
                        if was_running {
                            details.push("安装完成后将自动重启 Harness 使新版本生效".to_string());
                        }
                        crate::ops::set_details(&h, &details);
                        // 弹出向导窗口（旧实现只有 install 分支弹窗，sync-install 不弹 →
                        // 用户点「更新」后什么都看不到，这正是「更新成功没有任何提示」的根因）
                        if let Err(e) = crate::console::open_console(&h) {
                            log::warn!("更新向导窗口打开失败（降级为通知）：{e}");
                        }
                        crate::ops::mark_step_running(&h, 0);
                        crate::ops::update_step(&h, &format!("正在安装 {name_clone}…"));
                        match crate::sync::install_plugin(&h, &name).await {
                            Ok(()) => {
                                crate::ops::append_log(&h, &format!("✓ {name_clone} 已安装"));
                                // ③ 更新插件后必须重启 Harness 才生效 —— 自动完成，
                                //    不再让用户自己摸索「停止 + 启动」两次。
                                let restarted = if was_running {
                                    auto_restart_harness(&h, &name_clone, 1).await
                                } else {
                                    crate::ops::append_log(&h, "Harness 未在运行，无需重启（下次启动即加载新版本）");
                                    false
                                };
                                let summary = if restarted {
                                    format!("{name_clone} 已就绪，Harness 已自动重启生效")
                                } else {
                                    format!("{name_clone} 已就绪")
                                };
                                crate::ops::finish_op(&h, &summary);
                                notify(&h, title, &summary);
                                // 安装后立即同步一次（刷新状态 + 上报服务端；用缓存 TTL，不强制）
                                let cfg = load_cached();
                                let _ = crate::sync::sync_once(&h, &cfg, None, false).await;
                                refresh_sync_menu(&h);
                            }
                            Err(e) => {
                                // 失败必须可见：既有窗口展示，也有系统通知
                                crate::ops::fail_op(&h, &e);
                                notify(&h, &format!("{title}失败"), &e);
                                refresh_sync_menu(&h);
                            }
                        }
                    });
                }
            }
        }
        id if id.starts_with("sync-remove-") => {
            // 🗑 建议卸载（场景①：管理员已下架）——二次确认后卸载
            let idx = id
                .strip_prefix("sync-remove-")
                .and_then(|s| s.parse::<usize>().ok());
            if let Some(idx) = idx {
                if let Some(name) = removable_plugin_at(app, idx) {
                    if !confirm_uninstall(&name, "管理员已从服务端下架此插件") {
                        notify(app, "已取消", &format!("未卸载 {name}"));
                        return;
                    }
                    let h = app.clone();
                    let name_clone = name.clone();
                    tauri::async_runtime::spawn(async move {
                        crate::ops::start_op(&h, "plugin-uninstall", "卸载插件", &["卸载插件"]);
                        crate::ops::mark_step_running(&h, 0);
                        crate::ops::update_step(&h, &format!("正在卸载 {name_clone}…"));
                        match crate::sync::uninstall_plugin(&h, &name).await {
                            Ok(()) => {
                                crate::ops::finish_op(&h, &format!("{name_clone} 已卸载"));
                                notify(&h, "插件已卸载", &format!("{name_clone} 已卸载"));
                                let cfg = load_cached();
                                let _ = crate::sync::sync_once(&h, &cfg, None, false).await;
                                refresh_sync_menu(&h);
                            }
                            Err(e) => {
                                crate::ops::fail_op(&h, &e);
                                notify(&h, "插件卸载失败", &e);
                                refresh_sync_menu(&h);
                            }
                        }
                    });
                }
            }
        }
        id if id.starts_with("sync-broken-") => {
            // ⚠️ 影响启动（场景②：被 DSH 兜底禁用）——二次确认后清理
            let idx = id
                .strip_prefix("sync-broken-")
                .and_then(|s| s.parse::<usize>().ok());
            if let Some(idx) = idx {
                if let Some(name) = broken_plugin_at(app, idx) {
                    if !confirm_uninstall(&name, "此插件已被 DSH 禁用（影响启动）") {
                        notify(app, "已取消", &format!("未清理 {name}"));
                        return;
                    }
                    let h = app.clone();
                    let name_clone = name.clone();
                    tauri::async_runtime::spawn(async move {
                        crate::ops::start_op(&h, "plugin-uninstall", "清理插件", &["清理插件"]);
                        crate::ops::mark_step_running(&h, 0);
                        crate::ops::update_step(&h, &format!("正在清理 {name_clone}…"));
                        match crate::sync::uninstall_plugin(&h, &name).await {
                            Ok(()) => {
                                crate::ops::finish_op(&h, &format!("{name_clone} 已清理"));
                                notify(&h, "插件已清理", &format!("{name_clone} 已清理"));
                                refresh_sync_menu(&h);
                            }
                            Err(e) => {
                                crate::ops::fail_op(&h, &e);
                                notify(&h, "插件清理失败", &e);
                                refresh_sync_menu(&h);
                            }
                        }
                    });
                }
            }
        }
        id if id.starts_with("plugin-uninstall-") => {
            // 已装插件一键卸载（用户自主操作）：二次确认后卸载
            let idx = id
                .strip_prefix("plugin-uninstall-")
                .and_then(|s| s.parse::<usize>().ok());
            if let Some(idx) = idx {
                if let Some(name) = installed_plugin_at(app, idx) {
                    if !confirm_uninstall(&name, "卸载后该插件将从当前 profile 移除") {
                        notify(app, "已取消", &format!("未卸载 {name}"));
                        return;
                    }
                    let h = app.clone();
                    let name_clone = name.clone();
                    tauri::async_runtime::spawn(async move {
                        crate::ops::start_op(&h, "plugin-uninstall", "卸载插件", &["卸载插件"]);
                        crate::ops::mark_step_running(&h, 0);
                        crate::ops::update_step(&h, &format!("正在卸载 {name_clone}…"));
                        match crate::sync::uninstall_plugin(&h, &name).await {
                            Ok(()) => {
                                crate::ops::finish_op(&h, &format!("{name_clone} 已卸载"));
                                notify(&h, "插件已卸载", &format!("{name_clone} 已卸载"));
                                refresh_sync_menu(&h);
                            }
                            Err(e) => {
                                crate::ops::fail_op(&h, &e);
                                notify(&h, "插件卸载失败", &e);
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
        "logpack" => {
            // 收集日志到桌面 zip，并打开所在目录（小白可直接拖给管理员）
            let h = app.clone();
            tauri::async_runtime::spawn(async move {
                crate::ops::start_op(&h, "logpack", "收集日志", &[]);
                match crate::logpack::collect(&h) {
                    Ok(r) => {
                        let msg = format!(
                            "已收集 {} 个文件 → {}",
                            r.file_count,
                            r.zip_path.display()
                        );
                        crate::ops::finish_op(&h, &msg);
                        notify(&h, "日志已收集", &format!("已保存到桌面：{}", r.zip_path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()));
                        // 打开桌面目录，方便直接拖给管理员
                        if let Some(parent) = r.zip_path.parent() {
                            let _ = h.opener().open_path(parent.to_string_lossy().to_string(), None::<&str>);
                        }
                    }
                    Err(e) => {
                        crate::ops::fail_op(&h, &format!("收集日志失败：{e}"));
                        notify(&h, "收集日志失败", &e);
                    }
                }
            });
        }
        "cert-reinstall" => {
            // 重装内网根证书：独立重试入口（此前「同步」菜单并不触发证书安装，
            // 证书导入失败后用户无路可走；此菜单项补上真实闭环）。
            let h = app.clone();
            tauri::async_runtime::spawn(async move {
                crate::ops::start_op(&h, "cert-reinstall", "重装内网证书", &["导入根证书"]);
                crate::ops::mark_step_running(&h, 0);
                crate::ops::update_step(&h, "正在导入内网根证书…");
                let cfg = load_cached();
                match crate::install::install_root_ca(&h, &cfg) {
                    Ok(()) => {
                        crate::ops::finish_op(&h, "内网根证书已导入系统信任库");
                        notify(&h, "内网证书已就绪", "已导入系统信任库，浏览器访问 *.ai.ict.cmcc 不再红锁");
                        refresh_sync_menu(&h);
                    }
                    Err(e) => {
                        crate::ops::fail_op(&h, &e);
                        notify(&h, "内网证书导入失败", &format!("{e}\n\n若提示需要管理员权限，请以管理员身份重新运行 launcher 后再试"));
                        refresh_sync_menu(&h);
                    }
                }
            });
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
        "reset" => {
            // 预防性挡：有进行中的长操作则拒绝（避免清数据时被并发覆盖）
            if crate::ops::has_running() {
                notify(app, "一键重置", "已有操作进行中，请稍候再点");
                return;
            }
            // 二次确认：三选一（备份后清空 / 彻底清空 / 取消）
            let choice = crate::reset::confirm_reset_choice();
            match choice {
                crate::reset::ResetChoice::Cancel => {
                    notify(app, "一键重置", "已取消，未做任何改动");
                    return;
                }
                crate::reset::ResetChoice::BackupThenClear => {
                    log::info!("一键重置：选择「备份会话后清空」");
                }
                crate::reset::ResetChoice::Wipe => {
                    log::info!("一键重置：选择「彻底清空」");
                }
            }
            let h = app.clone();
            tauri::async_runtime::spawn(async move {
                crate::ops::start_op(
                    &h,
                    "reset",
                    "重置数字分身",
                    &["停止数字分身", "备份/清理数据"],
                );
                // 1) 先停 Harness（释放文件锁，否则删不掉正在用的文件）
                crate::ops::mark_step_running(&h, 0);
                crate::ops::update_step(&h, "正在停止数字分身…");
                crate::workflow::stop();
                std::thread::sleep(std::time::Duration::from_millis(600));

                let cfg = load_cached();
                // 2) 按选择决定是否先备份
                if choice == crate::reset::ResetChoice::BackupThenClear {
                    crate::ops::mark_step_running(&h, 1);
                    crate::ops::update_step(&h, "正在备份会话历史…");
                    match crate::reset::backup_user_data(&h, &cfg) {
                        Ok(dir) => {
                            log::info!("重置前会话已备份到 {}", dir.display());
                        }
                        Err(e) => {
                            // 备份失败不阻断清空，但如实记录
                            log::warn!("重置前备份会话失败（继续清空）：{e}");
                        }
                    }
                }
                // 3) 清空 DSH 用户数据
                crate::ops::mark_step_running(&h, 1);
                crate::ops::update_step(&h, "正在清空数字分身数据…");
                match crate::reset::clear_dsh_data(&h, &cfg) {
                    Ok(()) => {
                        let msg = if choice == crate::reset::ResetChoice::BackupThenClear {
                            "数字分身数据已清空，会话历史已保留。\n下次启动时会询问是否找回会话。"
                        } else {
                            "数字分身数据已彻底清空。\n重启 launcher 后重新安装即可。"
                        };
                        crate::ops::finish_op(&h, msg);
                        notify(&h, "重置完成", msg);
                    }
                    Err(e) => {
                        crate::ops::fail_op(&h, &e);
                        notify(&h, "重置失败", &e);
                    }
                }
                refresh_sync_menu(&h);
            });
        }
        "quit" => {
            QUIT_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
            crate::workflow::stop();
            app.exit(0);
        }
        _ => {}
    }
}

/// 请求应用退出（供自更新等内部流程调用：停 Harness + 真退出）。
pub fn request_quit<R: Runtime>(app: &AppHandle<R>) {
    QUIT_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
    crate::workflow::stop();
    app.exit(0);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 批量菜单文案：三种组合都要说清「更新几个 / 安装几个」。
    #[test]
    fn batch_menu_label_covers_three_cases() {
        // 全是更新
        let all_upd = batch_menu_label(3, 3, 0);
        assert!(all_upd.contains("全部更新") && all_upd.contains('3'), "{all_upd}");
        // 全是安装
        let all_ins = batch_menu_label(2, 0, 2);
        assert!(all_ins.contains("全部安装") && all_ins.contains('2'), "{all_ins}");
        // 混合：必须同时报出两个数，否则用户不知道有几个是升级
        let mixed = batch_menu_label(3, 2, 1);
        assert!(mixed.contains("更新 2") && mixed.contains("安装 1"), "{mixed}");
    }

    /// 批量步骤文案：更新与安装必须可区分（用户要看出这次是升级还是首装）。
    #[test]
    fn batch_step_label_distinguishes_update_and_install() {
        let upd = batch_step_label("dsh-himarket", "0.1.7", "0.1.8", true);
        assert_eq!(upd, "更新 dsh-himarket（0.1.7 → 0.1.8）");
        let ins = batch_step_label("dsh-new", "", "1.0.0", false);
        assert_eq!(ins, "安装 dsh-new（1.0.0）");
        assert_ne!(upd, ins);
    }

    /// ⭐ 核心红线：有失败时**绝不能**说「全部处理完成」。
    /// 否则用户以为全好了，实际有插件没装上——这正是「更新成功却没提示」的同类问题。
    #[test]
    fn batch_summary_never_claims_success_when_any_failed() {
        let failed = vec![("dsh-b".to_string(), "PLUGIN_INSTALL_FAILED: exit=1".to_string())];
        let s = batch_summary(3, 2, &failed, false);
        assert!(!s.contains("全部处理完成"), "有失败却说全部完成：{s}");
        assert!(s.contains("完成 2 个，失败 1 个"), "缺少计数：{s}");
        // 失败项必须带名字和真因，用户要据此决策
        assert!(s.contains("dsh-b"), "失败项未列出名字：{s}");
        assert!(s.contains("PLUGIN_INSTALL_FAILED"), "失败项未带真因：{s}");
    }

    /// 全成功时才说「全部处理完成」。
    #[test]
    fn batch_summary_all_success() {
        let s = batch_summary(3, 3, &[], false);
        assert!(s.contains("3 个插件全部处理完成"), "{s}");
        assert!(!s.contains("失败"), "{s}");
    }

    /// 只有真的重启了才说「已自动重启生效」，不谎报。
    #[test]
    fn batch_summary_reports_restart_honestly() {
        let no = batch_summary(2, 2, &[], false);
        assert!(!no.contains("已自动重启"), "没重启却说重启了：{no}");
        let yes = batch_summary(2, 2, &[], true);
        assert!(yes.contains("Harness 已自动重启生效"), "{yes}");
    }

    /// 多个失败项逐条列出（不能只给个数）。
    #[test]
    fn batch_summary_lists_every_failure() {
        let failed = vec![
            ("dsh-a".to_string(), "err-a".to_string()),
            ("dsh-b".to_string(), "err-b".to_string()),
        ];
        let s = batch_summary(2, 0, &failed, false);
        assert!(s.contains("失败 2 个"), "{s}");
        assert!(s.contains("dsh-a") && s.contains("err-a"), "{s}");
        assert!(s.contains("dsh-b") && s.contains("err-b"), "{s}");
    }
}
