//! 安装编排：按 Node → pnpm → dsh →（Windows）Git 顺序安装全部依赖，
//! 随后预置 web profile 插件与 matrix profile（数字分身），
//! 并为 matrix profile 写入自定义品牌名称 patch（pnpm shim + `dsh plugin add`）。

use std::path::PathBuf;
use std::process::{Command, Stdio};
use tauri::{AppHandle, Runtime};

use crate::config::*;
use crate::download::Component;

/// matrix profile（数字分身）的本地插件源名。
/// - dsh-matrix-agent：Matrix 桥接（已发布到 npmjs / npmmirror / 内网 registry.ict.cmcc）
/// - launcher-brand：内置品牌名称覆盖插件（file: 引用，随 launcher 分发）
pub const MATRIX_PROFILE: &str = "matrix";

/// matrix profile 必需的 bundle（两个，缺一不可，见 ensure_matrix_profile 文档）：
/// - `@deepseek-ai/dsh-base`：核心 service（sessionPersistence/sessions/commands/llm 等），
///   随 dsh 核心安装目录分发，由 fallback 软链，无需 plugin add。
/// - `@deepseek-ai/dsh-web-app`：前端/agent-presets/webserver 等 host 服务，独立 npm 包，
///   需真正 `dsh plugin add` 安装（本常量即其安装 spec）。
///
/// 版本选择：与 dsh 核心同一代（dsh 0.1.5-rc.1 ↔ 0.1.5-rc.2，均 0.1.5 线）。
/// 内网 Verdaccio（registry.ict.cmcc）实测有 0.1.5-rc.2，可装出完整依赖树。
pub const MATRIX_WEB_APP: &str = "@deepseek-ai/dsh-web-app@0.1.5-rc.2";

/// 安装 / 修复全部组件 + 预置 profile 插件。
pub async fn install_all<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let cfg = load_cached();

    // 操作状态中心：登记安装操作 + 自动弹出进度窗口
    // 证书导入放在第一步：它是独立于依赖下载的最快闭环，且此前排在最末位
    // 会被前面任何一步失败「短路」——证书永远装不上，浏览器一直红锁。
    let steps = vec![
        "导入内网根证书",
        "下载 / 安装 Node.js",
        "安装 pnpm",
        "下载 Harness 核心",
        "预置 profile 插件",
        "安装服务器推荐插件",
    ];
    crate::ops::start_op(app, "install", "安装 / 修复", &steps);
    if let Err(e) = crate::console::open_console(app) {
        log::warn!("进度窗口打开失败（降级为托盘状态 + 通知）：{e}");
    }

    // ① 内网根证书：最先做。失败不阻断后续依赖安装（依赖本体仍要装好），
    //    但必须显式告知用户并留下重试入口（托盘「重装内网证书」）。
    crate::ops::mark_step_running(app, 0);
    crate::ops::update_step(app, "正在导入内网根证书…");
    crate::ops::append_log(app, "开始导入内网根证书（im.ai.ict.cmcc 等 HTTPS 依赖）…");
    if let Err(e) = install_root_ca(app, &cfg) {
        log::warn!("内网根证书导入未完成：{e}");
        crate::ops::mark_step_failed(app, 0);
        crate::ops::append_log(app, &format!("✗ 内网根证书导入未完成：{e}"));
        crate::notify::notify(
            app,
            "内网证书导入未完成",
            &format!("{e}\n\n依赖安装会继续；完成后可点托盘「重装内网证书」补装"),
        );
    } else {
        crate::ops::append_log(app, "✓ 内网根证书已导入系统信任库");
    }

    // 预写 npmrc，使加速源在安装后就绪（供后续插件拉包）
    let _ = crate::plugin::ensure_profile_npmrc(app, &cfg);

    let mut order = vec![Component::Node, Component::Pnpm, Component::Dsh];
    #[cfg(windows)]
    order.push(Component::Git);

    // 步骤 0 已被「导入内网根证书」占用，组件从步骤 1 开始
    let mut step_index = 1usize;
    for component in &order {
        if component.check_installed(app) {
            log::info!("{} 已安装，跳过", component.title());
            continue;
        }
        crate::ops::mark_step_running(app, step_index);
        crate::ops::update_step(app, &format!("正在安装 {}…", component.title()));
        crate::ops::append_log(app, &format!("开始安装 {}…", component.title()));
        crate::notify::notify(app, "安装 / 修复", &format!("正在安装 {}…", component.title()));

        // 下载进度回调 → 更新窗口状态（如 "正在下载 Node.js 45%"）
        let h = app.clone();
        let comp_title = component.title().to_string();
        let result = component.install(app, Some(&move |downloaded, total| {
            let pct = if total > 0 {
                (downloaded as f64 / total as f64 * 100.0).round() as u32
            } else {
                0
            };
            crate::ops::update_step(&h, &format!("正在下载 {comp_title} {pct}%"));
        }))
        .await;

        match result {
            Ok(()) => {
                crate::ops::append_log(app, &format!("✓ {} 安装完成", component.title()));
                crate::ops::update_step(app, &format!("✓ {} 安装完成", component.title()));
                crate::notify::notify(app, "安装 / 修复", &format!("{} 安装完成", component.title()));
            }
            Err(e) => {
                crate::ops::mark_step_failed(app, step_index);
                crate::ops::fail_op(app, &format!("{} 安装失败：{e}", component.title()));
                crate::notify::notify(app, "安装失败", &format!("{}：{e}", component.title()));
                return Err(e);
            }
        }
        step_index += 1;
    }

    // 安装完成后再次确保 npmrc 生效
    crate::plugin::ensure_profile_npmrc(app, &cfg)?;

    // Windows 下补 pnpm.cmd shim（dsh plugin 内部以 shell:true 调 pnpm，
    // cmd.exe 找不到裸 pnpm.cjs；pnpm 官方安装包仅含 pnpm.cjs）
    ensure_pnpm_shim(app);

    // 把 launcher-brand 插件复制到 <dsh_home>/launcher-brand（供 file: 引用）。
    // 注意：这里只【释放】本地插件源；是否装进哪个 profile 由服务端清单决定
    // （见下 install_server_recommended），不再代码里硬编码预置任何插件。
    copy_launcher_brand(app, &cfg);

    // 预置当前生效 profile 的插件（服务器清单驱动，选中哪个 profile 就装哪个）。
    // 合并了原「预置 web」「预置 matrix」两步——预置来源从代码硬编码改为服务端下发。
    crate::ops::mark_step_running(app, 4);
    crate::ops::update_step(app, "预置 profile 插件…");
    crate::notify::notify(app, "安装 / 修复", "预置 profile 插件…");
    preset_current_profile(app, &cfg).await?;

    // 服务器推荐的插件装到当前生效 profile（同事实际运行的 profile）。
    // 与上一步「预置 profile 插件」共用一个清单来源（profilePlugins 优先、plugins 兜底），
    // 这里负责补装/更新（含 registry 最新版比对），失败不阻断。
    crate::ops::mark_step_running(app, 5);
    crate::ops::update_step(app, "安装服务器推荐插件…");
    crate::notify::notify(app, "安装 / 修复", "安装服务器推荐插件…");
    install_server_recommended(app, &cfg).await?;

    // 补齐缺失的 bundle patch：npm 发布的 bundle 可能声明 dsh.bundle.patch
    // 但实际没打包该文件（如 dsh-matrix-agent 0.2.1），dsh 启动读 overlay 崩溃。
    // 自动创建最小 patch（insert + disabled，用户配置后再启用）。
    repair_missing_bundle_patches(app, &cfg);

    crate::ops::finish_op(app, "DeepSeek Harness 及依赖已就绪");
    crate::notify::notify(app, "安装完成", "DeepSeek Harness 及依赖已就绪");
    log::info!("全部依赖安装完成");
    Ok(())
}

/// 把服务器推荐的、当前 profile 未装或版本落后的插件安装/更新到当前 profile。
/// 失败不阻断（仅日志+通知），避免单个插件问题拖垮整个安装流程。
pub async fn install_server_recommended<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> Result<(), String> {
    let server_url = resolve_server_url(cfg);
    if server_url.is_empty() {
        log::info!("未配置服务端，跳过服务器推荐插件安装");
        return Ok(());
    }
    // 用本地缓存的推荐清单（sync 循环已缓存；离线也能装上次拉到的）。
    // 清单按当前 profile 精确取：profilePlugins[当前] 优先，回落全局 plugins。
    let profile = resolve_profile(cfg);
    let state = crate::sync::load_state(app, cfg);
    let Some(recommended) = state
        .cached_config
        .as_ref()
        .map(|c| crate::sync::plugins_for_profile(c, &profile))
    else {
        log::info!("暂无服务端推荐清单缓存，跳过");
        return Ok(());
    };
    // 待处理 = 未装 + 已装旧版（版本信息来自同步缓存的 registry 最新版）
    let installed_with_ver = crate::sync::installed_plugins_current_profile_with_versions(app, cfg);
    let entries = crate::sync::pending_with_updates(&recommended, &installed_with_ver, &state.plugin_latest_versions);
    // 构造安装 spec：未装 → 裸名（装最新）；已装旧版 → name@<latest>（显式版本，pnpm 才升级）
    let specs: Vec<String> = entries
        .iter()
        .filter_map(|e| {
            let name = e["name"].as_str()?;
            if e["action"].as_str() == Some("update") {
                let latest = e["latest"].as_str().unwrap_or("latest");
                Some(if latest.is_empty() || latest == "latest" {
                    format!("{name}@latest")
                } else {
                    format!("{name}@{latest}")
                })
            } else {
                Some(name.to_string())
            }
        })
        .collect();
    if specs.is_empty() {
        log::info!("当前 profile 无待装/待更新推荐插件");
        return Ok(());
    }
    log::info!("安装/更新服务器推荐插件到 {profile}：{}", specs.join(", "));
    let result = preset_profile(app, cfg, &profile, &specs).await;
    // 安装结果不影响整体完成（失败有日志 + 托盘可重试）
    if let Err(e) = &result {
        log::error!("服务器推荐插件安装失败：{e}");
        crate::notify::notify(app, "推荐插件安装未完成", &format!("部分插件安装失败，可在托盘「同步 / 推荐插件」重试：{e}"));
    }
    // 安装后刷新托盘（pending 归零则不再提示待装）
    crate::tray::refresh_sync_menu(app);
    result
}

/// 切换 profile 后调用：让当前 profile 的插件清单与服务端对齐。
///
/// 流程（实现「选中哪个 profile 就该是哪个」）：
/// 1. 强制同步一次（拉到服务端最新 profilePlugins，按当前 profile 计算 pending/removed）；
/// 2. 安装当前 profile 清单里待装/待更新的插件；
/// 3. 自动卸载「曾经的预置插件、本次清单已下架」的插件（口径 A：只在 server_seen
///    集合里出现过的才卸，同事自己手动装的绝不碰）。
///
/// 失败不阻断（仅日志 + 通知）：单个插件问题不应拖垮 profile 切换本身。
pub async fn apply_profile_plugins<R: Runtime>(app: &AppHandle<R>) {
    let cfg = load_cached();
    if resolve_server_url(&cfg).is_empty() {
        log::info!("未配置服务端，跳过 profile 插件对齐");
        return;
    }

    // 1. 强制同步（拉最新配置；force=true 忽略 registry 版本缓存）
    let outcome = crate::sync::sync_once(app, &cfg, None, true).await;

    // 2. 安装当前 profile 待装/待更新插件
    if let Err(e) = install_server_recommended(app, &cfg).await {
        log::warn!("profile 插件安装未完成（不阻断）：{e}");
    }

    // 3. 自动卸载「曾经预置、现已下架」的插件（口径 A）
    //    注意：这里只对「曾在服务端清单里出现过」的插件自动卸载——
    //    同事自己手动装的插件从未进过 server_seen_plugins，不会被碰。
    let removed = outcome.removed;
    if !removed.is_empty() {
        let profile = resolve_profile(&cfg);
        let installed = crate::sync::installed_plugins_current_profile(app, &cfg);
        let installed_set: std::collections::HashSet<&str> =
            installed.iter().map(|s| s.as_str()).collect();
        for name in &removed {
            if !installed_set.contains(name.as_str()) {
                continue; // 已不在当前 profile，无需卸载
            }
            match crate::sync::uninstall_plugin(app, name).await {
                Ok(()) => {
                    log::info!("已自动卸载下架的预置插件：{name}（profile={profile}）");
                    crate::notify::notify(
                        app,
                        "插件已自动卸载",
                        &format!("服务端已下架 {name}，已从 {profile} profile 卸载"),
                    );
                }
                Err(e) => {
                    log::warn!("自动卸载 {name} 失败（不阻断）：{e}");
                }
            }
        }
    }

    crate::tray::refresh_sync_menu(app);
}

/// 补齐缺失的 bundle patch 文件。
///
/// 背景：bundle 插件的 package.json 声明 `dsh.bundle.patch: ./cordis.patch.yml`，
/// 但 npm 发布时可能没把该文件打进去（实测 dsh-matrix-agent 所有版本都缺）。
/// dsh 启动时 `loadOverlayPatches` 读不到文件直接 throw → 整个插件树加载失败，
/// 表现为「提示启动成功但打不开网页」。
///
/// 处理：遍历 profile 的 node_modules 下所有 bundle 声明插件，检测 patch 文件
/// 缺失 → 自动创建最小 patch（`insert` 新 entry + `disabled: true`，避免插件
/// 因缺少必需配置（如 Matrix token）在启动时抛错拖垮整棵树）。
/// 用户配置好后在 profile 的 cordis.patch.yml 覆盖 disabled: false 即可启用。
fn repair_missing_bundle_patches<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) {
    let profiles_dir = dsh_home(app, cfg).join("profiles");
    let Ok(entries) = std::fs::read_dir(&profiles_dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let profile_dir = entry.path();
        let nm = profile_dir.join("node_modules");
        // 扫描 node_modules 顶层 + @scope 下的包（返回完整包名 name@scope 形式）
        let scan = |dir: &PathBuf, scope: Option<&str>| -> Vec<(String, PathBuf)> {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return Vec::new();
            };
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| {
                    let pkg_json = e.path().join("package.json");
                    if !pkg_json.is_file() {
                        return None;
                    }
                    let text = std::fs::read_to_string(&pkg_json).ok()?;
                    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
                    // 仅收集声明了 dsh.bundle.patch 的包
                    let has_patch = json
                        .get("dsh")
                        .and_then(|d| d.get("bundle"))
                        .and_then(|b| b.get("patch"))
                        .and_then(|p| p.as_str())
                        .is_some();
                    if !has_patch {
                        return None;
                    }
                    let leaf = e.file_name().to_string_lossy().to_string();
                    let full = match scope {
                        Some(s) => format!("@{s}/{leaf}"),
                        None => leaf.clone(),
                    };
                    Some((full, e.path()))
                })
                .collect()
        };
        let mut bundles = scan(&nm, None);
        if let Ok(scoped) = std::fs::read_dir(nm.join("@deepseek-ai")) {
            for s in scoped.flatten() {
                if s.path().is_dir() {
                    bundles.extend(scan(&s.path(), Some("deepseek-ai")));
                }
            }
        }
        for (name, pkg_dir) in bundles {
            // 从 package.json 读 patch 相对路径
            let text = std::fs::read_to_string(pkg_dir.join("package.json")).unwrap_or_default();
            let json: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
            let Some(patch_rel) = json
                .get("dsh")
                .and_then(|d| d.get("bundle"))
                .and_then(|b| b.get("patch"))
                .and_then(|p| p.as_str())
            else {
                continue;
            };
            let patch_path = pkg_dir.join(patch_rel.trim_start_matches("./"));
            let missing = !patch_path.exists();
            if missing {
                // bundle 层 patch：insert entry（id=包名，config 空）。
                // 不 disabled——禁用/配置由 profile 层覆盖（同 id，避免重复 entry）。
                let content = format!(
                    "# {name} bundle 层（launcher 自动补：npm 发布缺此文件导致 dsh 启动崩溃）\n\
                     # 此文件 insert entry；是否禁用/配置由 profile 层 cordis.patch.yml 覆盖。\n\
                     - insert:\n\
                     \x20   - id: {name}\n\
                     \x20     name: {name}\n\
                     \x20     config: {{}}\n"
                );
                if let Some(parent) = patch_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match std::fs::write(&patch_path, content) {
                    Ok(()) => log::info!("已自动补缺失 bundle patch：{}", patch_path.display()),
                    Err(e) => log::warn!("补 bundle patch 失败 {}：{e}", patch_path.display()),
                }

                // profile 层：追加「已装未启用」覆盖块（id + config 覆盖，含占位 token 提示）。
                // 用户可见可编辑——插件保持启用（设置页可见可配置），但用占位值避免
                // 缺必需参数（如 accessToken）导致 dsh 启动崩溃。配好参数后覆盖生效。
                // 仅在 patch 缺失（launcher 补的）时才登记，正常 bundle 不动。
                let profile_patch = profile_dir.join("cordis.patch.yml");
                let mut existing = std::fs::read_to_string(&profile_patch).unwrap_or_default();
                let marker = format!("{name}（launcher 自动补）");
                if !existing.contains(&marker) {
                    let block = format!(
                        "\n# ── {marker} ──\n\
                         # npm 发布缺 bundle patch，launcher 自动补；插件保持启用（设置页可配置），\n\
                         # 但必需参数未配置前用占位值避免启动崩溃。配好后覆盖对应字段。\n\
                         - id: {name}\n\
                         \x20 name: {name}\n\
                         \x20 config:\n\
                         \x20   accessToken: 'pending-config'\n\
                         \x20   homeserverUrl: ''\n\
                         \x20   userId: ''\n\
                         \x20   owner: ''\n"
                    );
                    existing.push_str(&block);
                    match std::fs::write(&profile_patch, existing) {
                        Ok(()) => log::info!("已在 profile 层登记未启用插件：{name}"),
                        Err(e) => log::warn!("写 profile patch 失败：{e}"),
                    }
                }
            }
        }
    }
}

/// 把 launcher-brand 插件目录复制到 `<dsh_home>/launcher-brand`。
///
/// ⚠️ 2026-09-14 修复：此前用 `env!("CARGO_MANIFEST_DIR")` 定位源目录——
/// 那是**编译期**路径，会被烧进二进制。本机开发时指向 `E:\ai-works\...`，
/// 同事机器上该路径根本不存在 → 日志出现
/// `launcher-brand 插件源缺失：E:\ai-works\deepseek-harness-launcher\launcher-brand`，
/// 进而跳过 matrix profile 预置（数字分身 profile 建不起来）。
///
/// 现在按**运行时**顺序找，第一个命中的即用：
/// 1. exe 同级 `launcher-brand/`（发布包结构：zip 里 exe 与 launcher-brand 同级）
/// 2. exe 同级 `resources/launcher-brand/`（Tauri bundle 结构）
/// 3. `<dsh_home>/launcher-brand`（已复制过，幂等直接跳过复制）
/// 4. 编译期路径（仅本机 `cargo run`/开发调试时有效，放最后兜底）
fn launcher_brand_src<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    // 1/2. 以 exe 位置为基准（发布包内 exe 与 launcher-brand 同级）
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("launcher-brand"));
            candidates.push(dir.join("resources").join("launcher-brand"));
        }
    }
    // 3. 已经落到 dsh_home 的副本
    candidates.push(dsh_home(app, cfg).join("launcher-brand"));
    // 4. 开发期兜底（编译期路径）
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(|p| p.join("launcher-brand"))
            .unwrap_or_else(|| PathBuf::from("launcher-brand")),
    );

    candidates
        .into_iter()
        .find(|p| p.join("package.json").exists())
}

/// launcher-brand 插件：**内嵌进二进制**（编译期 include_str!）。
///
/// 为什么内嵌而不是随包分发：launcher 支持**自动更新**，而更新下载的是
/// **单个 exe**（`launcher-<ver>.exe` 替换自身）。若 launcher-brand 只放在
/// zip 里 exe 旁边，自动更新后该目录不存在 → 插件预置被跳过
/// （同事机器日志：`launcher-brand 插件源缺失：E:\ai-works\...`，2026-09-14）。
/// 内嵌后无论 exe 怎么被替换/搬移，都能自解压出插件，彻底消除该故障。
///
/// 体积代价：约 4KB（4 个小文件）。
const LAUNCHER_BRAND_FILES: &[(&str, &str)] = &[
    (
        "package.json",
        include_str!("../../launcher-brand/package.json"),
    ),
    (
        "cordis.patch.yml",
        include_str!("../../launcher-brand/cordis.patch.yml"),
    ),
    ("lib/index.js", include_str!("../../launcher-brand/lib/index.js")),
    (
        "lib/client.js",
        include_str!("../../launcher-brand/lib/client.js"),
    ),
];

/// 把内嵌的 launcher-brand 释放到 `<dsh_home>/launcher-brand`。
///
/// 幂等：内容有变化才重写（用「全部存在且内容一致」判断，避免每次都写盘）。
/// 返回 true 表示目标目录可用。
fn materialize_launcher_brand<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> bool {
    let dest = dsh_home(app, cfg).join("launcher-brand");
    for (rel, content) in LAUNCHER_BRAND_FILES {
        let path = dest.join(rel);
        // 内容已一致 → 跳过写入（幂等，减少磁盘操作）
        if let Ok(existing) = std::fs::read_to_string(&path) {
            if existing == *content {
                continue;
            }
        }
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                log::warn!("创建 launcher-brand 目录失败：{e}");
                return false;
            }
        }
        if let Err(e) = std::fs::write(&path, content) {
            log::warn!("写出 launcher-brand 文件失败（{}）：{e}", path.display());
            return false;
        }
    }
    dest.join("package.json").exists()
}

/// 把 launcher-brand 插件目录准备到 `<dsh_home>/launcher-brand`。
///
/// 优先级：
/// 1. **内嵌内容**（首选——自动更新后仍然可用，见上方说明）
/// 2. 磁盘上的 launcher-brand（exe 同级 / resources / 编译期路径）——
///    仅在 dsh_home 副本已存在且内嵌释放失败时兜底
fn copy_launcher_brand<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) {
    let dest = dsh_home(app, cfg).join("launcher-brand");

    // 首选：从二进制内嵌内容释放（不依赖任何外部文件）
    if materialize_launcher_brand(app, cfg) {
        log::info!("launcher-brand 已就绪（内嵌释放）：{}", dest.display());
        return;
    }

    // 兜底：从磁盘上的源目录复制
    let Some(src) = launcher_brand_src(app, cfg) else {
        log::warn!("launcher-brand 释放失败且磁盘无源目录，跳过");
        return;
    };
    if src == dest {
        log::warn!("launcher-brand 内嵌释放失败（目标目录不完整）：{}", dest.display());
        return;
    }
    if let Err(e) = copy_dir_recursive(&src, &dest) {
        log::warn!("复制 launcher-brand 失败：{e}");
        return;
    }
    log::info!("launcher-brand 已就绪：{}（源 {}）", dest.display(), src.display());
}

/// 递归复制目录（覆盖）。
///
/// ⚠️ 必须先判 symlink 再判 dir：Windows junction 用 `Path::is_dir()` 判定为 true
/// （跟随 reparse point），会被当作真实目录递归复制 → **解引用物化**。
/// 用 `symlink_metadata` 拿到「不跟随」的类型，才能把链接原样复制。
fn copy_dir_recursive(src: &PathBuf, dest: &PathBuf) -> Result<(), String> {
    if dest.exists() {
        std::fs::remove_dir_all(dest).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let ty = std::fs::symlink_metadata(&from)
            .map_err(|e| e.to_string())?
            .file_type();
        if ty.is_symlink() {
            copy_link(&from, &to)?;
        } else if ty.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// 复制一个符号链接/junction 本身（不解引用）。
/// 用 junction 重建：`symlink_dir` 需要管理员权限（os error 1314），
/// 普通同事机器上会失败，故走免特权的 mklink /J。
fn copy_link(from: &PathBuf, to: &PathBuf) -> Result<(), String> {
    let target = std::fs::read_link(from).map_err(|e| e.to_string())?;
    crate::config::create_dir_link(&target, to)
}

/// 内嵌的内网根 CA 证书（编译期 include_str!）。
///
/// 用途：im.ai.ict.cmcc 等 *.ai.ict.cmcc 域名已启用 HTTPS（自建内网 CA 签发，
/// 零成本、不依赖集团 CMCA）。浏览器（Chrome/Edge）默认不信任该根 CA，
/// 故需在首次安装时把它导入 Windows 系统信任库，否则访问会报红锁。
///
/// 内嵌原因与 launcher-brand 相同：launcher 支持自动更新（替换单个 exe），
/// 若证书只放在 zip 里 exe 旁边，自动更新后丢失 → 同事机器红锁复发。
const ICT_INTERNAL_CA_PEM: &str = include_str!("../resources/ict-internal-ca.crt");

/// 把内嵌的内网根 CA 导入 Windows 系统信任库（Chrome/Edge 走这里）。
///
/// 流程（先查后装，避免重复弹 UAC）：
/// 1. 证书落到磁盘（certutil 需要文件路径）
/// 2. **只读检查** `certutil -store Root`：若已含本根 CA → 直接 Ok，不弹 UAC
/// 3. **UAC 提权导入**：`ShellExecuteExW("runas")` 拉起提权的 cmd 执行
///    `certutil -addstore -f Root <cert>`，弹系统原生 UAC 对话框
/// 4. 回读验证信任库，确认确实已入「受信任的根证书颁发机构」
///
/// 返回 `Result<(), String>`：Ok = 已成功导入（或验证确认已存在）；Err = 导入失败，
/// 错误信息已尽量精确定位（用户取消 UAC / certutil 缺失 / 证书内容异常）。
/// 供 install_all 与托盘「重装内网证书」菜单共用，保证两处行为一致。
pub fn install_root_ca<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> Result<(), String> {
    #[cfg(windows)]
    {
        // 1. 落到磁盘（certutil 需要一个文件路径）
        let cert_path = dsh_home(app, cfg).join("ict-internal-ca.crt");
        if let Some(parent) = cert_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // 内容有变化才重写（幂等，避免无谓磁盘 IO）
        let need_write = match std::fs::read_to_string(&cert_path) {
            Ok(existing) => existing != ICT_INTERNAL_CA_PEM,
            Err(_) => true,
        };
        if need_write {
            std::fs::write(&cert_path, ICT_INTERNAL_CA_PEM)
                .map_err(|e| format!("写入根证书失败：{e}"))?;
        }

        // 2. 只读预检：证书已在信任库 → 直接成功，不弹 UAC。
        //    certutil -store Root 是只读操作，不需要管理员权限。
        match certutil_has_root_ca() {
            Ok(true) => {
                log::info!("内网根 CA 已存在于系统信任库，跳过导入");
                return Ok(());
            }
            Ok(false) => {
                log::info!("内网根 CA 不在信任库，进入 UAC 提权导入");
            }
            Err(e) => {
                // 预检失败（如 certutil 缺失）不能直接当作「没装」——
                // 下面提权导入会再次暴露真实错误。这里仅记录，继续走导入流程。
                log::warn!("预检系统信任库失败（继续尝试导入）：{e}");
            }
        }

        // 2.5 去静默化：在弹 UAC 前先弹一个「说明 + 确认」的原生对话框，
        //     讲清楚接下来会发生什么（提权导入内网根证书）、为什么需要、
        //     以及选择「否」的后果。避免用户只看到一闪的 UAC、不明所以，
        //     也降低「静默提权」被安全软件误判为可疑程序的可能。
        if !confirm_root_ca_import() {
            return Err(
                "已取消：你选择不导入内网根证书。可随时在托盘「重装内网证书」重试"
                    .to_string(),
            );
        }

        // 3. UAC 提权导入：用 ShellExecuteExW 的 runas verb 拉起提权进程执行
        //    certutil -addstore -f Root <cert>。弹系统原生 UAC 对话框。
        //    Chrome/Edge 在 Windows 上读系统信任库，导入后即绿锁。
        //    Firefox 用独立 NSS 库，暂不处理（同事主要用 Chrome/Edge）。
        elevate_certutil_addstore(&cert_path)?;

        // 4. 验证闭环：读回系统信任库，确认证书确实在「受信任的根证书颁发机构」里。
        //    不验证的话，certutil 报成功但实际没进信任库（例如被组策略拦截）时会误报绿锁。
        match certutil_has_root_ca() {
            Ok(true) => {
                log::info!("验证通过：内网根 CA 已在系统信任库中");
                Ok(())
            }
            Ok(false) => Err(
                "验证失败：导入命令已执行，但未在系统信任库中找到「ICT Internal AI Root CA」"
                    .to_string(),
            ),
            Err(e) => Err(format!("验证系统信任库失败：{e}")),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (app, cfg);
        log::info!("非 Windows 平台，跳过根 CA 导入");
        Ok(())
    }
}

/// 导入根 CA 前的说明确认框：返回 true = 继续导入，false = 用户取消。
///
/// 目的（去静默化）：让用户在被 UAC 提权前**先知道**接下来会发生什么——
/// 程序要把内网根证书加入系统「受信任的根证书颁发机构」，需要管理员权限。
/// 讲清「为什么需要」和「点否的后果」，避免只看到一闪的 UAC、不明所以，
/// 也降低「静默提权」这一动作被安全软件（Windows Defender 等）误判为可疑行为的可能。
#[cfg(windows)]
fn confirm_root_ca_import() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OKCANCEL, IDOK};
    let title: Vec<u16> = "安装内网安全证书"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let body: Vec<u16> = format!(
        "为了让浏览器正常访问公司内网站点（*.ai.ict.cmcc 等，否则会显示“不安全/红锁”），\
         需要把内网根证书「ICT Internal AI Root CA」加入系统信任库。\n\n\
         下一步 Windows 会弹出“用户账户控制(UAC)”授权框，点击“是”即完成安装（仅这一次，之后不再提示）。\n\n\
         · 点击「确定」= 继续，随后在 UAC 弹窗中选择「是」\n\
         · 点击「取消」= 跳过，不导入（可稍后在托盘“重装内网证书”重试）"
    )
    .encode_utf16()
    .chain(std::iter::once(0))
    .collect();
    let ret = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            body.as_ptr(),
            title.as_ptr(),
            MB_OKCANCEL | MB_ICONINFORMATION,
        )
    };
    ret == IDOK
}

/// 非 Windows 平台：无确认框，直接视为继续（桌面端仅面向 Windows）。
#[cfg(not(windows))]
fn confirm_root_ca_import() -> bool {
    true
}

/// 只读检查系统信任库是否已含本内网根 CA（按 CN 精确匹配，避免误判同名证书）。
///
/// `certutil -store Root` 是只读操作，不需要管理员权限，因此可放心前置预检。
/// 返回 Ok(true) = 已存在；Ok(false) = 不存在；Err = 查询失败（如 certutil 缺失）。
#[cfg(windows)]
fn certutil_has_root_ca() -> Result<bool, String> {
    let out = Command::new("certutil")
        .arg("-store")
        .arg("Root")
        .output()
        .map_err(|e| format!("调用 certutil 失败（系统可能缺少 certutil）：{e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        return Err(format!("certutil -store Root 返回非零：{}", stderr.trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    Ok(text.contains("ICT Internal AI Root CA"))
}

/// 通过 UAC 提权执行 `certutil -addstore -f Root <cert>`。
///
/// 用 `ShellExecuteExW` 的 `runas` verb 拉起**提权的** `cmd.exe /C certutil ...`，
/// 触发系统原生 UAC 对话框。launcher 本体保持普通权限，只在导入这一瞬间提权。
///
/// 关键行为：
/// - 同步等待：`ShellExecuteExW` 返回 `hProcess` 进程句柄，`WaitForSingleObject`
///   等提权进程结束，否则无法立即回读验证；
/// - 用户点「否」/关闭 UAC → `ShellExecuteExW` 返回失败且 `GetLastError` =
///   `ERROR_CANCELLED`(1223) → 友好提示；
/// - `SW_HIDE` 隐藏 cmd 窗口，避免闪黑框。
#[cfg(windows)]
fn elevate_certutil_addstore(cert_path: &std::path::Path) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_CANCELLED};
    use windows_sys::Win32::System::Threading::WaitForSingleObject;
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, SHELLEXECUTEINFOW_0,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    // 命令：certutil -addstore -f Root "<cert>"  —— 用引号包裹证书路径，
    // 避免路径含空格时被 cmd 拆错。整个命令经 runas 提权执行。
    let cert = cert_path.to_string_lossy();
    let cmd_line = format!("certutil -addstore -f Root \"{cert}\"");

    // 宽字符（Windows 需 UTF-16 且以 NUL 结尾的参数）
    let operation: Vec<u16> = "runas".encode_utf16().chain(std::iter::once(0)).collect();
    let file: Vec<u16> = "cmd.exe".encode_utf16().chain(std::iter::once(0)).collect();
    let params: Vec<u16> = format!("/C \"{cmd_line}\"")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    log::info!("弹 UAC 提权导入根 CA：{cmd_line}");

    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS, // 要拿到 hProcess，才能等待进程结束
        hwnd: std::ptr::null_mut(),
        lpVerb: operation.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: params.as_ptr(),
        lpDirectory: std::ptr::null(),
        nShow: SW_HIDE as i32,
        hInstApp: std::ptr::null_mut(),
        lpIDList: std::ptr::null_mut(),
        lpClass: std::ptr::null(),
        hkeyClass: std::ptr::null_mut(), // HKEY = *mut c_void
        dwHotKey: 0,
        Anonymous: SHELLEXECUTEINFOW_0 {
            hMonitor: std::ptr::null_mut(),
        },
        hProcess: std::ptr::null_mut(),
    };

    let ok = unsafe { ShellExecuteExW(&mut sei) };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        if err == ERROR_CANCELLED {
            return Err(
                "已取消：你在系统提示中选择了「否」，未导入内网证书。可在托盘「重装内网证书」重试"
                    .to_string(),
            );
        }
        return Err(format!("UAC 提权启动失败（错误码 {err}）：无法弹出管理员授权对话框"));
    }

    // 同步等待提权进程结束（certutil 导入是毫秒级，但必须等它写完才能回读验证）
    if !sei.hProcess.is_null() {
        unsafe {
            // 无限等待；certutil 导入极快，正常情况下秒级返回
            WaitForSingleObject(sei.hProcess, 0xFFFFFFFF);
            CloseHandle(sei.hProcess);
        }
    }

    Ok(())
}

/// Windows 下确保 `pnpm.cmd` 存在（转发到同目录 pnpm.cjs）。
fn ensure_pnpm_shim<R: Runtime>(app: &AppHandle<R>) {
    #[cfg(windows)]
    {
        let bin_dir = pnpm_install_path(app).join("bin");
        let cmd_path = bin_dir.join("pnpm.cmd");
        if cmd_path.exists() {
            return;
        }
        let _ = std::fs::create_dir_all(&bin_dir);
        let shim = "@ECHO OFF\r\nnode \"%~dp0pnpm.cjs\" %*\r\n";
        if let Err(e) = std::fs::write(&cmd_path, shim) {
            log::warn!("写入 pnpm.cmd shim 失败：{e}");
        } else {
            log::info!("已写入 pnpm.cmd shim：{}", cmd_path.display());
        }
    }
}

/// 预置指定 profile 的插件：`node <dsh>/lib/bin.js plugin --profile <name> add <pkg>...`。
///
/// 失败不阻断安装流程（依赖本体已装好），错误信息返回给调用方通知。
pub async fn preset_profile<R: Runtime>(
    app: &AppHandle<R>,
    cfg: &LauncherConfig,
    profile: &str,
    packages: &[String],
) -> Result<(), String> {
    let node = effective_node_path(app, cfg);
    let dsh_bin = dsh_binary_path(app);
    if !node.exists() || !dsh_bin.exists() {
        log::warn!("Node 或 dsh 核心未就绪，跳过插件预置");
        return Ok(());
    }

    let env = crate::workflow::child_env(app, cfg)?;
    let mut cmd = Command::new(&node);
    cmd.arg(&dsh_bin)
        .arg("plugin")
        .arg("--profile")
        .arg(profile)
        .arg("add");
    for p in packages {
        cmd.arg(p);
    }
    cmd.current_dir(dsh_install_path(app));
    for (k, v) in &env {
        cmd.env(k, v);
    }
    // 插件安装要能看到输出，失败时日志可查
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    log::info!("预置 {profile} profile 插件：{}", packages.join(", "));
    let output = tauri::async_runtime::spawn_blocking(move || cmd.output()).await
        .map_err(|e| format!("PRESET_SPAWN_FAILED: {e}"))?;
    let output = output.map_err(|e| format!("PRESET_LAUNCH_FAILED: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::error!("插件预置失败（exit={}）：{}", output.status, stderr.trim());
        return Err(format!(
            "PLUGIN_PRESET_FAILED: 预置 {profile} 插件未完成（exit={}），可稍后在托盘重试。详情见日志",
            output.status
        ));
    }
    log::info!("{profile} profile 插件预置完成");
    Ok(())
}

/// 预置当前生效 profile 的结构性骨架（不含"装哪些插件"——那由服务端清单决定）。
///
/// 职责：
/// - 通用：预写当前 profile 的 .npmrc（加速源）；
/// - matrix profile 额外：释放 launcher-brand（file: 本地源）→ 加入 builtin bundle
///   `@deepseek-ai/dsh-web-app` → 写品牌 patch → 下发环境默认配置。
///
/// 注意：不再在此处硬编码 add 任何插件。插件清单统一由服务端 `profilePlugins`
/// （回落 `plugins`）驱动，在 `install_server_recommended` 中安装/更新。
pub async fn preset_current_profile<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> Result<(), String> {
    let profile = resolve_profile(cfg);
    // 通用：预写当前 profile 的 .npmrc
    let _ = crate::plugin::ensure_profile_npmrc_for(app, cfg, &profile);

    // matrix profile：额外做结构性骨架（launcher-brand 释放已在 install_all 前置完成）
    // 注意：ensure_matrix_profile 现在会真正安装 web-app（不只是写 bundles 声明）。
    if profile == MATRIX_PROFILE {
        preset_matrix_profile(app, cfg).await?;
    }

    log::info!("当前 profile {profile} 骨架预置完成（插件由服务端清单驱动）");
    Ok(())
}

/// matrix profile 的结构性骨架（含 web-app 真正安装）：
/// 1. 把 `@deepseek-ai/dsh-web-app` 加进 bundles（声明）
/// 2. 真正 `dsh plugin add` 安装 web-app（提供 agent-presets / webserver 等 host 服务）
/// 3. 写 cordis.patch.yml（配置品牌名称）
/// 4. 下发环境默认配置（各内网服务地址等统一值）到 settings.yaml
///
/// launcher-brand / dsh-matrix-agent 等插件本身由服务端 profilePlugins.matrix 清单
/// 决定是否安装（见 install_server_recommended）。
pub async fn preset_matrix_profile<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> Result<(), String> {
    ensure_matrix_profile(app, cfg)
}

/// 确保 matrix profile 存在（同步、幂等、可重复调用）。
///
/// 这是「数字分身能启动」的最小前置：dsh 启动 `--profile matrix` 时要求
/// `profiles/matrix/package.json` 已存在，否则直接报
/// `profile "matrix" does not exist`（进程秒退 → 端口不就绪 → HARNESS_NOT_READY）。
///
/// 背景（2026-09-23 同事故障）：0.4.7 把「dsh 已装但未激活」分流为「直接打开激活
/// 向导」，跳过 `install_all`，导致 `preset_current_profile` 从未执行 → matrix profile
/// 从未创建。激活流程（run_activation）只写 settings.yaml 账号、不建 profile，于是
/// `launch_with_profile("matrix")` 报错。修复 = 在任何进入激活向导/启动分身的路径上，
/// 先确保 profile 骨架存在。
///
/// 幂等：profile 已存在则只补缺（add_builtin_bundle 检测 bundles 是否已含目标项；
/// write_matrix_brand_patch 直接覆盖写；env_defaults 只填空缺不覆盖用户值）。
pub fn ensure_matrix_profile<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> Result<(), String> {
    // ⭐ 根因修复（2026-09-23 事故）：matrix profile 的 bundles 必须同时含
    //    `@deepseek-ai/dsh-base` 和 `@deepseek-ai/dsh-web-app` 两个 bundle，缺一不可：
    //    - dsh-base：提供 sessionPersistence / sessions / commands / credentials /
    //      storageDomain / llm / tools 等**核心 service**。dsh 启动时 17 个官方插件
    //      （dsh-agent / dsh-agent-loop / dsh-api-session-controller 等）都依赖这些 service。
    //      缺 dsh-base → 全员 pending → assertEntriesActivated 报「17 entries did not
    //      activate」→ 进程退出 → 端口 90s 不就绪（HARNESS_NOT_READY）。
    //      dsh-base 随 dsh 核心安装目录分发（dsh/node_modules/@deepseek-ai/dsh-base），
    //      由 healProfilesModuleFallback 软链到 profiles/node_modules，**无需 plugin add**。
    //    - dsh-web-app：提供 agent-presets / webserver / 前端 UI 等 host 服务。它是**独立
    //      npm 包**（不在 dsh 核心安装目录内），必须真正 `dsh plugin add` 安装（见
    //      install_matrix_web_app）——旧注释「内置 bundle 从安装目录解析、不需要 pnpm 安装」
    //      在 dsh@0.1.5-rc.1 上不成立。
    //    顺序：dsh-base 在前（核心 service 先行），web-app 在后。
    add_builtin_bundle(app, cfg, MATRIX_PROFILE, "@deepseek-ai/dsh-base")?;
    add_builtin_bundle(app, cfg, MATRIX_PROFILE, "@deepseek-ai/dsh-web-app")?;

    // 真正安装 web-app（独立 npm 包，dsh-base 已随核心分发无需装）。
    install_matrix_web_app(app, cfg)?;

    // 写品牌 patch（配置品牌名称）
    write_matrix_brand_patch(app, cfg)?;

    // 下发环境默认配置（各内网服务地址等统一值）到 settings.yaml。
    // 遵循「只填空缺」：用户已显式设置过的不覆盖（详见 env_defaults 模块文档）。
    // 失败不阻断预置——插件缺地址时保持存活并在设置页可补填。
    match crate::env_defaults::apply_env_defaults_to_file(
        &crate::matrix_setup::settings_yaml_path(app, cfg),
    ) {
        Ok((filled, skipped)) => {
            if filled > 0 {
                log::info!("已下发环境默认配置：填充 {filled} 项，保留用户已设 {skipped} 项");
            }
        }
        Err(e) => log::warn!("下发环境默认配置失败（不阻断）：{e}"),
    }

    Ok(())
}

/// 真正安装 matrix profile 的 web-app bundle（同步执行 `dsh plugin add`）。
///
/// 与 `preset_profile`（async）不同，本函数同步阻塞执行——因为 `ensure_matrix_profile`
/// 的调用点（first_run 的 spawn、/activate 与 /submit 的 spawn_blocking）本就在工作线程
/// 上，阻塞等待插件安装完成是预期行为，且必须先装完再 launch，否则分身启动即失败。
///
/// 失败**阻断** ensure_matrix_profile（返回 Err）：web-app 缺失会导致分身 100% 启动失败，
/// 属于硬前置，不能像普通推荐插件那样「失败不阻断」。调用方会记日志并提示用户。
fn install_matrix_web_app<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> Result<(), String> {
    let node = effective_node_path(app, cfg);
    let dsh_bin = dsh_binary_path(app);
    if !node.exists() || !dsh_bin.exists() {
        log::warn!("Node 或 dsh 核心未就绪，跳过 web-app 安装（install_all 会兜底装）");
        return Ok(());
    }

    let env = crate::workflow::child_env(app, cfg)?;
    let mut cmd = Command::new(&node);
    cmd.arg(&dsh_bin)
        .arg("plugin")
        .arg("--profile")
        .arg(MATRIX_PROFILE)
        .arg("add")
        .arg(MATRIX_WEB_APP);
    cmd.current_dir(dsh_install_path(app));
    for (k, v) in &env {
        cmd.env(k, v);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    log::info!("安装 matrix profile web-app bundle：{MATRIX_WEB_APP}");
    let output = cmd.output().map_err(|e| format!("WEBAPP_INSTALL_SPAWN_FAILED: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        log::error!(
            "web-app 安装失败（exit={}）：stdout={} stderr={}",
            output.status,
            stdout.trim(),
            stderr.trim()
        );
        return Err(format!(
            "WEBAPP_INSTALL_FAILED: 安装 @deepseek-ai/dsh-web-app 未完成（exit={}），数字分身依赖它才能启动",
            output.status
        ));
    }
    log::info!("matrix profile web-app bundle 安装完成");
    Ok(())
}

/// 把 dsh 内置 bundle（如 dsh-web-app）加入 profile 的 `dsh.profile.bundles`。
///
/// 注意（2026-09-23 修正）：只写 bundles 声明**不足以保证 bundle 可用**。web-app 是独立
/// npm 包（不在 dsh 核心安装目录内），必须在 bundles 声明之外再真正 `dsh plugin add`
/// 安装——见 `install_matrix_web_app`。本函数仅负责 manifest 层声明，不负责安装。
fn add_builtin_bundle<R: Runtime>(
    app: &AppHandle<R>,
    cfg: &LauncherConfig,
    profile: &str,
    bundle: &str,
) -> Result<(), String> {
    let manifest_path = dsh_home(app, cfg)
        .join("profiles")
        .join(profile)
        .join("package.json");
    // manifest 不存在（profile 尚未初始化）→ 先建最小骨架，再往下写 bundles。
    // 之前靠「先 add launcher-brand 触发 profile 初始化」保证 manifest 存在；
    // 现在插件安装由服务端清单驱动、与骨架解耦，故在此兜底创建。
    if !manifest_path.exists() {
        if let Some(parent) = manifest_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("PROFILE_DIR_CREATE_FAILED: {e}"))?;
        }
        let minimal = serde_json::json!({
            "name": format!("dsh-profile-{profile}"),
            "private": true,
            "dsh": { "profile": { "bundles": [] } }
        });
        std::fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&minimal)
                .map_err(|e| format!("PROFILE_MANIFEST_SERIALIZE_FAILED: {e}"))?,
        )
        .map_err(|e| format!("PROFILE_MANIFEST_WRITE_FAILED: {e}"))?;
        log::info!("已初始化 {profile} profile manifest（最小骨架）");
    }
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("PROFILE_MANIFEST_READ_FAILED: {e}"))?;
    let mut json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("PROFILE_MANIFEST_PARSE_FAILED: {e}"))?;
    let bundles = json
        .get_mut("dsh")
        .and_then(|d| d.get_mut("profile"))
        .and_then(|p| p.get_mut("bundles"))
        .and_then(|b| b.as_array_mut())
        .ok_or("PROFILE_MANIFEST_NO_BUNDLES: dsh.profile.bundles 缺失")?;
    let exists = bundles.iter().any(|v| v.as_str() == Some(bundle));
    if !exists {
        bundles.push(serde_json::Value::String(bundle.to_string()));
        let json_out = serde_json::to_string_pretty(&json)
            .map_err(|e| format!("PROFILE_MANIFEST_SERIALIZE_FAILED: {e}"))?;
        std::fs::write(&manifest_path, json_out)
            .map_err(|e| format!("PROFILE_MANIFEST_WRITE_FAILED: {e}"))?;
        log::info!("已将内置 bundle {bundle} 加入 {profile} profile");
    }
    Ok(())
}

/// 写 matrix profile 的 cordis.patch.yml：配置 launcher-brand 品牌名称。
///
/// 注意：matrix profile 无 dsh-web-app（纯数字分身进程，无 web UI），
/// 因此**不**禁用 ui-brand-official（该行在此 profile 中不存在，patch 会报错）。
/// launcher-brand 的 brandName 仅在 profile 未来挂载 web-app 时用于 UI 区分。
fn write_matrix_brand_patch<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> Result<(), String> {
    let patch_path = dsh_home(app, cfg)
        .join("profiles")
        .join(MATRIX_PROFILE)
        .join("cordis.patch.yml");
    if let Some(parent) = patch_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let content = "# matrix profile 用户补丁层（launcher 维护）\n\
                   # 数字分身进程：纯 Matrix 桥，无 web UI。\n\
                   # launcher-brand 随 bundle 注入（若未来挂 web-app 则显示自定义品牌名）。\n\
                   - id: launcher-brand\n\
                   \x20 name: launcher-brand\n\
                   \x20 config:\n\
                   \x20   brandName: '数字分身'\n";
    std::fs::write(&patch_path, content).map_err(|e| format!("MATRIX_PATCH_WRITE_FAILED: {e}"))?;
    log::info!("已写入 matrix profile 品牌 patch：{}", patch_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归测试：launcher-brand 必须**内嵌在二进制里**。
    ///
    /// 2026-09-14 同事故障：该插件原以 `env!("CARGO_MANIFEST_DIR")` 定位磁盘目录，
    /// 编译期路径被烧进 exe（指向开发者机器的 `E:\ai-works\...`），
    /// 同事机器上不存在 → 日志「launcher-brand 插件源缺失」→ 跳过 profile 预置。
    /// 且 launcher 自动更新只替换单个 exe，磁盘目录方案在更新后必然失效。
    #[test]
    fn launcher_brand_is_embedded() {
        // 4 个必需文件全部内嵌
        let names: Vec<&str> = LAUNCHER_BRAND_FILES.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"package.json"), "缺 package.json");
        assert!(names.contains(&"cordis.patch.yml"), "缺 cordis.patch.yml");
        assert!(names.contains(&"lib/index.js"), "缺 lib/index.js");
        assert!(names.contains(&"lib/client.js"), "缺 lib/client.js");

        // 内容非空且是有效 JSON / YAML 形态
        for (name, content) in LAUNCHER_BRAND_FILES {
            assert!(!content.trim().is_empty(), "{name} 内容为空");
        }
        let (_, pkg) = LAUNCHER_BRAND_FILES
            .iter()
            .find(|(n, _)| *n == "package.json")
            .expect("package.json 应存在");
        let json: serde_json::Value = serde_json::from_str(pkg).expect("package.json 应是合法 JSON");
        assert_eq!(json["name"], "launcher-brand");
        // dsh bundle patch 声明（dsh 靠它找 cordis.patch.yml）
        assert_eq!(json["dsh"]["bundle"]["patch"], "./cordis.patch.yml");
    }

    /// 内嵌文件不应包含编译期绝对路径（防止再次把开发机路径带进发布物）。
    #[test]
    fn launcher_brand_has_no_compiled_paths() {
        for (name, content) in LAUNCHER_BRAND_FILES {
            assert!(
                !content.contains("ai-works"),
                "{name} 含开发机路径 ai-works，发布物会带出本机路径"
            );
        }
    }
}
