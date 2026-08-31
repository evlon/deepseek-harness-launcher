//! 安装编排：按 Node → pnpm → dsh →（Windows）Git 顺序安装全部依赖，
//! 随后预置 web profile 插件与 matrix profile（数字分身），
//! 并为 matrix profile 写入自定义品牌名称 patch（pnpm shim + `dsh plugin add`）。

use std::path::PathBuf;
use std::process::{Command, Stdio};
use tauri::{AppHandle, Runtime};

use crate::config::*;
use crate::download::Component;

/// 预置插件清单（对齐桌面端 web profile 的常用插件）。
/// 首个 add 会自动初始化 web profile（dsh-base + dsh-web-app）。
/// 全部不带版本号：`dsh plugin add <pkg>` 默认装最新版；包会持续更新，
/// 已装的保持现状（如需升级由服务端推荐/手动 add 新版本触发）。
pub const PRESET_PLUGINS: &[&str] = &[
    "dsh-codebuddy-models",
    "dsh-nested-followups",
    "dsh-plugin-message-rewrite",
    "@noob-stupid/dsh-plugin-console",
];

/// matrix profile（数字分身）的本地插件源。
/// - dsh-matrix-agent：Matrix 桥接（已发布到 npmjs / npmmirror / 内网 registry.ict.cmcc）
/// - launcher-brand：内置品牌名称覆盖插件（file: 引用，随 launcher 分发）
pub const MATRIX_PROFILE: &str = "matrix";

/// dsh-matrix-agent 的 npm 包名（从 registry 安装，不带版本 = 最新版）。
pub const MATRIX_AGENT_PACKAGE: &str = "dsh-matrix-agent";

/// 安装 / 修复全部组件 + 预置 profile 插件。
pub async fn install_all<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let cfg = load_cached();

    // 操作状态中心：登记安装操作 + 自动弹出进度窗口
    let steps = vec![
        "下载 / 安装 Node.js",
        "安装 pnpm",
        "下载 Harness 核心",
        "预置 web 插件",
        "预置 matrix 数字分身",
        "安装服务器推荐插件",
    ];
    crate::ops::start_op(app, "install", "安装 / 修复", &steps);
    if let Err(e) = crate::console::open_console(app) {
        log::warn!("进度窗口打开失败（降级为托盘状态 + 通知）：{e}");
    }

    // 预写 npmrc，使加速源在安装后就绪（供后续插件拉包）
    let _ = crate::plugin::ensure_profile_npmrc(app, &cfg);

    let mut order = vec![Component::Node, Component::Pnpm, Component::Dsh];
    #[cfg(windows)]
    order.push(Component::Git);

    let mut step_index = 0usize;
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

    // 把 launcher-brand 插件复制到 <dsh_home>/launcher-brand（供 file: 引用）
    copy_launcher_brand(app, &cfg);

    // 预置 web profile 插件（幂等：已装的自动跳过/更新）
    crate::ops::mark_step_running(app, 3);
    crate::ops::update_step(app, "预置 web 插件…");
    crate::notify::notify(app, "安装 / 修复", "预置 web 插件…");
    let web_packages: Vec<String> = PRESET_PLUGINS.iter().map(|s| s.to_string()).collect();
    preset_profile(app, &cfg, "web", &web_packages).await?;

    // 预置 matrix profile（数字分身）：dsh-matrix-agent + launcher-brand + 品牌 patch
    crate::ops::mark_step_running(app, 4);
    crate::ops::update_step(app, "预置 matrix 数字分身…");
    crate::notify::notify(app, "安装 / 修复", "预置 matrix 数字分身…");
    preset_matrix_profile(app, &cfg).await?;

    // 服务器推荐的插件装到当前生效 profile（同事实际运行的 profile）。
    // 默认 profile 是 matrix，服务器推荐的 web 类插件（如 dsh-codebuddy-models）
    // 必须装到这里才会在运行实例里生效——装到 web profile 对 matrix 用户不可见。
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
async fn install_server_recommended<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> Result<(), String> {
    let server_url = resolve_server_url(cfg);
    if server_url.is_empty() {
        log::info!("未配置服务端，跳过服务器推荐插件安装");
        return Ok(());
    }
    // 用本地缓存的推荐清单（sync 循环已缓存；离线也能装上次拉到的）
    let state = crate::sync::load_state(app, cfg);
    let Some(recommended) = state.cached_config.as_ref().map(|c| c.plugins.clone()) else {
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
    let profile = resolve_profile(cfg);
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

                // profile 层：追加「已装未启用」覆盖块（disabled: true + 配置指引），
                // 用户可见可编辑——配置好必需项后把 disabled: true 改 false 即启用。
                // 仅在 patch 缺失（launcher 补的）时才登记，正常 bundle 不动。
                let profile_patch = profile_dir.join("cordis.patch.yml");
                let mut existing = std::fs::read_to_string(&profile_patch).unwrap_or_default();
                let marker = format!("{name}（launcher 自动补）");
                if !existing.contains(&marker) {
                    let block = format!(
                        "\n# ── {marker} ──\n\
                         # npm 发布缺 bundle patch，launcher 自动补；未配置必需项前禁用。\n\
                         # 配置方法：设置页配置后，把下面 disabled: true 改为 false。\n\
                         - id: {name}\n\
                         \x20 name: {name}\n\
                         \x20 disabled: true\n\
                         \x20 config: {{}}\n"
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
fn copy_launcher_brand<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fallback = PathBuf::from(".");
    let src = manifest_dir
        .parent()
        .unwrap_or(&fallback)
        .join("launcher-brand");
    let dest = dsh_home(app, cfg).join("launcher-brand");
    if !src.join("package.json").exists() {
        log::warn!("launcher-brand 插件源缺失：{}", src.display());
        return;
    }
    if let Err(e) = copy_dir_recursive(&src, &dest) {
        log::warn!("复制 launcher-brand 失败：{e}");
        return;
    }
    log::info!("launcher-brand 已就绪：{}", dest.display());
}

/// 递归复制目录（覆盖）。
fn copy_dir_recursive(src: &PathBuf, dest: &PathBuf) -> Result<(), String> {
    if dest.exists() {
        std::fs::remove_dir_all(dest).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| e.to_string())?;
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

/// 预置 matrix profile（数字分身）：
/// 1. 写 .npmrc（加速源）
/// 2. `dsh plugin --profile matrix add launcher-brand`（file: 本地插件，先建 profile）
/// 3. 把 `@deepseek-ai/dsh-web-app` 加进 bundles（dsh 内置 bundle，从 dsh 安装目录解析，
///    无需 pnpm 安装——registry 上的 dsh-web-app 是旧版 0.0.1-rc.1）
/// 4. `dsh plugin --profile matrix add dsh-matrix-agent`（link: 本地包）
/// 5. 写 cordis.patch.yml（配置品牌名称）
pub async fn preset_matrix_profile<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> Result<(), String> {
    let brand_src = dsh_home(app, cfg).join("launcher-brand");

    if !brand_src.join("package.json").exists() {
        log::warn!("launcher-brand 未就绪，跳过 matrix profile 预置");
        return Ok(());
    }

    // 预写 matrix profile 的 .npmrc（确保拉包走加速源/内网 registry.ict.cmcc）
    let _ = crate::plugin::ensure_profile_npmrc_for(app, cfg, MATRIX_PROFILE);

    // 装 launcher-brand（file:）——触发 profile 初始化
    let brand_spec = format!("file:{}", brand_src.to_string_lossy());
    preset_profile(app, cfg, MATRIX_PROFILE, &[brand_spec]).await?;

    // 装 dsh-matrix-agent（从 registry 安装：npmjs / npmmirror / 内网 registry.ict.cmcc
    // 均有发布；npm_config_registry 指向哪个源由用户配置或地域决定）
    preset_profile(app, cfg, MATRIX_PROFILE, &[MATRIX_AGENT_PACKAGE.to_string()]).await?;

    // 把 @deepseek-ai/dsh-web-app 加入 bundles（dsh 内置，从安装目录解析；提供
    // agent-presets / webserver 等 host 服务，dsh-matrix-agent 依赖它们）
    add_builtin_bundle(app, cfg, MATRIX_PROFILE, "@deepseek-ai/dsh-web-app")?;

    // 写品牌 patch（配置品牌名称）
    write_matrix_brand_patch(app, cfg)?;
    Ok(())
}

/// 把 dsh 内置 bundle（如 dsh-web-app）加入 profile 的 `dsh.profile.bundles`。
///
/// 内置 bundle 从 dsh 安装目录解析（`resolveBundleDir` 先查 installAnchor），
/// 不需要也不应该 pnpm 安装（registry 上无对应新版）。
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
