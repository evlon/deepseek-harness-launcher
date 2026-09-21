//! launcher 自身（exe）内网自动更新 —— 轻量方案（版本号 + sha256 校验）。
//!
//! 背景：企业内网无外网，同事的 launcher 是**便携绿色单 exe**（解压即用、无安装器），
//! 由管理员经中心服务端（ai-conf）发布新版本 exe + 元数据；launcher 周期轮询
//! `GET {serverUrl}/api/launcher/latest` 发现新版 → 下载 → sha256 校验 → 替换自身 exe → 重启。
//!
//! 替换机制（Windows：运行中 exe 可改名、不可删除/覆盖）：
//! 1. 主进程下载新 exe 到 `{exe 同目录}\launcher-update-<ver>.exe.new`（或临时目录）
//! 2. spawn `当前exe --cmd update-self --update-file <新exe>`（走 CLI 分支，绕开
//!    single-instance 插件），随后主进程正常退出（释放 exe 文件锁）
//! 3. CLI 助手进程内：等待旧进程退出 → rename 旧 exe → `.old` 让出原名 →
//!    rename 新 exe → 原名位置 → 清理 `.old` → 重新 spawn 正常模式（无 --cmd）→ 退出
//!
//! 安全取舍：sha256 防损坏/防错放；不防内网攻击者篡改（与现有 http registry 信任
//! 边界一致）。如需防篡改后续可叠 minisign 校验或走 https（Caddy 已反代）。

use tauri::{AppHandle, Runtime};
use std::path::{Path, PathBuf};
use sha2::{Digest, Sha256};

/// 当前进程 exe 路径（std::env::current_exe）。
pub fn current_exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("deepseek-harness-launcher.exe"))
}

/// 当前 launcher 版本（编译期 CARGO_PKG_VERSION，与上报/元数据比对同源）。
pub fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// 服务端最新发布元数据（GET /api/launcher/latest 响应结构）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ReleaseMeta {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub published_at: Option<String>,
    /// 服务端无发布时返回 {"noRelease":true}
    #[serde(default)]
    pub no_release: bool,
}

/// 拉取服务端最新发布元数据（免鉴权）。
pub async fn fetch_latest<R: Runtime>(_app: &AppHandle<R>) -> Result<Option<ReleaseMeta>, String> {
    let cfg = crate::config::load_cached();
    let server_url = crate::config::resolve_server_url(&cfg);
    if server_url.is_empty() {
        return Ok(None);
    }
    let url = format!("{}/api/launcher/latest", server_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("self-update client: {e}"))?;
    let resp = client.get(&url).send().await.map_err(|e| format!("查询 launcher 更新失败：{e}"))?;
    if resp.status() == reqwest::StatusCode::NO_CONTENT {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Ok(None); // 服务端 4xx/5xx（含尚未部署该端点）→ 视为无更新，不报错打扰
    }
    let meta: ReleaseMeta = resp.json().await.map_err(|e| format!("解析更新元数据失败：{e}"))?;
    if meta.no_release || meta.version.is_empty() || meta.file.is_empty() {
        return Ok(None);
    }
    Ok(Some(meta))
}

/// 是否需要更新：远程版本 > 当前版本（严格大于；同版本不重复更新）。
pub fn has_newer(remote: &str) -> bool {
    let cur = current_version();
    if remote == cur {
        return false;
    }
    let parse = |v: &str| -> Option<semver::Version> {
        let v = v.trim().trim_start_matches('v');
        // 兼容不带 patch 的简写（如 "0.3"）
        semver::Version::parse(v).ok().or_else(|| {
            let mut parts = v.split('.').collect::<Vec<_>>();
            while parts.len() < 3 {
                parts.push("0");
            }
            semver::Version::parse(&parts.join(".")).ok()
        })
    };
    match (parse(&cur), parse(remote)) {
        (Some(a), Some(b)) => b > a,
        _ => true, // 解析失败保守视为需要更新（服务端元数据可信优先）
    }
}

/// 一次「检查更新」的结果（**只描述发现，不含任何下载/替换动作**）。
///
/// 抽出来是为了让三条路径（启动自检 / 托盘手动 / 周期轮询）共用同一份判定，
/// 且托盘菜单能在不发起网络请求的情况下渲染上次结果。
#[derive(Debug, Clone, PartialEq)]
pub enum CheckOutcome {
    /// 已是最新。
    UpToDate { current: String },
    /// 发现新版（尚未下载）。
    Available { current: String, version: String, notes: String, size: u64 },
    /// 服务端尚无任何发布（未部署该端点也算）。
    NoRelease { current: String },
    /// 检查失败（网络不可达等）。**不视为「已是最新」**——两者对用户含义不同。
    Failed { error: String },
}

impl CheckOutcome {
    /// 托盘菜单里的一行摘要。
    pub fn menu_label(&self) -> String {
        match self {
            CheckOutcome::UpToDate { current } => format!("launcher v{current}（已是最新）"),
            CheckOutcome::Available { version, .. } => format!("🚀 发现新版 v{version}（点击更新）"),
            CheckOutcome::NoRelease { current } => format!("launcher v{current}（服务端无发布）"),
            CheckOutcome::Failed { .. } => "launcher 更新检查失败（点击重试）".to_string(),
        }
    }

    /// 面向用户的完整文案（通知/进度窗口用）。
    pub fn message(&self) -> String {
        match self {
            CheckOutcome::UpToDate { current } => format!("当前已是最新版本 v{current}"),
            CheckOutcome::Available { current, version, notes, size } => {
                let mb = size / 1024 / 1024;
                let n = if notes.is_empty() { String::new() } else { format!("（{notes}）") };
                format!("发现新版本 v{version}{n}，当前 v{current}，约 {mb} MB，将自动更新")
            }
            CheckOutcome::NoRelease { current } => {
                format!("当前 v{current}；服务端暂未发布 launcher 版本")
            }
            CheckOutcome::Failed { error } => format!("检查更新失败：{error}"),
        }
    }

    /// 是否发现了可更新的新版。
    pub fn has_update(&self) -> bool {
        matches!(self, CheckOutcome::Available { .. })
    }
}

/// 最近一次检查结果（进程内缓存，托盘菜单渲染用）。值为 (结果, 检查时间 ISO)。
static LAST_CHECK: std::sync::Mutex<Option<(CheckOutcome, String)>> = std::sync::Mutex::new(None);

/// 记录一次检查结果（供托盘读取）。
fn remember(outcome: &CheckOutcome) {
    if let Ok(mut g) = LAST_CHECK.lock() {
        *g = Some((outcome.clone(), chrono::Utc::now().to_rfc3339()));
    }
}

/// 读最近一次检查结果（含时间）。尚未检查过返回 None。
pub fn last_check() -> Option<(CheckOutcome, String)> {
    LAST_CHECK.lock().ok().and_then(|g| g.clone())
}

/// 测试用：清空缓存的结果。
#[cfg(test)]
pub fn reset_last_check() {
    if let Ok(mut g) = LAST_CHECK.lock() {
        *g = None;
    }
}

/// **只检查、不下载**：拉服务端元数据并判定是否需要更新。
///
/// 与 `check_and_update` 的区别：本函数永不触发下载/替换/退出，可安全地用于
/// 「启动时看一眼有没有新版」以及托盘菜单渲染。结果同时写入进程内缓存。
pub async fn check_only<R: Runtime>(app: &AppHandle<R>) -> CheckOutcome {
    let (outcome, _) = fetch_and_classify(app).await;
    outcome
}

/// 拉一次元数据并分类，**同时返回原始元数据**（避免调用方二次请求）。
/// 结果写入进程内缓存。
async fn fetch_and_classify<R: Runtime>(app: &AppHandle<R>) -> (CheckOutcome, Option<ReleaseMeta>) {
    let (outcome, meta) = match fetch_latest(app).await {
        Ok(None) => (CheckOutcome::NoRelease { current: current_version() }, None),
        Ok(Some(meta)) => {
            let outcome = if has_newer(&meta.version) {
                CheckOutcome::Available {
                    current: current_version(),
                    version: meta.version.clone(),
                    notes: meta.notes.clone(),
                    size: meta.size,
                }
            } else {
                CheckOutcome::UpToDate { current: current_version() }
            };
            (outcome, Some(meta))
        }
        Err(e) => (CheckOutcome::Failed { error: e }, None),
    };
    remember(&outcome);
    log::info!("launcher 更新检查：{}", outcome.message());
    (outcome, meta)
}

/// 从服务端下载新版 exe 到目标路径并做 sha256 校验。
/// 返回下载字节数。
pub async fn download_release<R: Runtime>(
    _app: &AppHandle<R>,
    meta: &ReleaseMeta,
    dest: &Path,
) -> Result<u64, String> {
    let cfg = crate::config::load_cached();
    let server_url = crate::config::resolve_server_url(&cfg);
    if server_url.is_empty() {
        return Err("未配置 serverUrl，无法下载更新".to_string());
    }
    let url = format!(
        "{}/api/launcher/download?file={}",
        server_url.trim_end_matches('/'),
        meta.file
    );
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("download client: {e}"))?;
    log::info!("下载 launcher 新版：{url}");
    let resp = client.get(&url).send().await.map_err(|e| format!("下载失败：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("下载失败：HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| format!("读取下载内容失败：{e}"))?;
    // sha256 校验（复用 download.rs 的 verify_sha256）
    if !meta.sha256.is_empty() {
        crate::download::verify_sha256(&bytes, &meta.sha256)
            .map_err(|e| format!("launcher 更新包校验失败：{e}"))?;
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(dest, &bytes).map_err(|e| format!("写入更新包失败：{e}"))?;
    Ok(bytes.len() as u64)
}

/// 触发更新：spawn 自身 CLI 助手（--cmd update-self）执行替换+重启，然后请求本进程退出。
/// 返回 Err 表示触发失败（未启动替换）。
pub fn spawn_update_self<R: Runtime>(
    _app: &AppHandle<R>,
    downloaded: &Path,
) -> Result<(), String> {
    let cur = current_exe();
    let new_exe_abs = if downloaded.is_absolute() {
        downloaded.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(downloaded)
    };
    // 助手 = 当前 exe 自身（CLI 分支 --cmd update-self --update-file <新exe>）
    let mut cmd = std::process::Command::new(&cur);
    cmd.arg("--cmd")
        .arg("update-self")
        .arg("--update-file")
        .arg(&new_exe_abs);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW：替换过程无窗口
    }
    cmd.spawn().map_err(|e| format!("启动更新助手失败：{e}"))?;
    log::info!("更新助手已启动（{} → {}）", cur.display(), new_exe_abs.display());
    Ok(())
}

/// CLI `update-self` 助手执行体：等旧进程退出 → 替换 exe → 重启。
/// 由 main.rs 的 CLI 分支调用（此时本进程已是最新下载的 exe 的「先导」——见下注释）。
///
/// 说明：为绕开 Windows 的 exe 文件锁，采用「新 exe 自举」方式：
/// 下载的新 exe 以临时名落盘后，直接 spawn **新 exe** 带 `--cmd update-self --update-file <自己>`，
/// 新进程启动成功即说明下载文件完整可执行 → 新进程把旧的当前 exe rename 成 .old →
/// 把自己 rename 成正式名 → 清 .old → spawn 正式名（无 --cmd）→ 退出。
/// 返回是否应继续退出（0=正常完成）。
pub fn run_update_self(update_file: &str) -> i32 {
    log::info!("update-self 助手启动：update-file={update_file}");
    let cur = current_exe();
    let new_file = PathBuf::from(update_file);
    // 1) 等待旧进程（spawn 我们的父进程）退出释放文件锁——短暂等待即可（spawn 后父进程随即退出）
    std::thread::sleep(std::time::Duration::from_millis(800));

    // 目标 = 当前 exe 同目录 + 正式文件名
    let exe_name = cur.file_name().and_then(|n| n.to_str()).unwrap_or("deepseek-harness-launcher.exe");
    let dir = cur.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    let target = dir.join(exe_name);
    let old_backup = dir.join(format!(".{exe_name}.old"));

    // 2) 重试把旧 exe rename 让位（可能锁未完全释放，重试最多 30 次 * 300ms）
    let rename_old = |src: &Path, dst: &Path| -> std::io::Result<()> {
        if dst.exists() {
            let _ = std::fs::remove_file(dst);
        }
        std::fs::rename(src, dst)
    };
    let mut replaced = false;
    for attempt in 0..30 {
        // 把当前运行中的自己（旧版 exe）让位
        if rename_old(&target, &old_backup).is_ok() {
            // 3) 新文件 rename 到正式名
            match std::fs::rename(&new_file, &target) {
                Ok(()) => {
                    replaced = true;
                    break;
                }
                Err(e) => {
                    // 回滚：把旧 exe 挪回来
                    let _ = std::fs::rename(&old_backup, &target);
                    log::warn!("update-self 替换失败（第 {attempt} 次）：{e}");
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    if !replaced {
        log::error!("update-self 替换失败：无法替换 {}（锁未释放？）", target.display());
        // 尽力恢复：新文件保留为 .new，下次启动重试
        return 1;
    }

    // 4) 清理备份
    let _ = std::fs::remove_file(&old_backup);
    log::info!("launcher 已更新为 {}", target.display());

    // 5) 重启正式 exe（无 --cmd → 正常常驻模式）
    let mut cmd = std::process::Command::new(&target);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    match cmd.spawn() {
        Ok(_) => log::info!("新 launcher 已启动"),
        Err(e) => log::error!("重启新 launcher 失败：{e}（可手动双击启动）"),
    }
    0
}

/// 周期自更新检查循环：**启动后立即检查一次**（这是用户要的「每次启动检查更新」），
/// 之后每 6 小时查一次服务端 /api/launcher/latest。
///
/// 启动那一轮做两件事：
///   ① `check_only` 先判定并写缓存 —— 让托盘菜单立刻能显示「已是最新 / 发现新版」；
///   ② 若发现新版，走自动下载替换（`check_and_update`），并**全程可见**：
///      操作进度窗口 + 托盘状态行 + 系统通知。
///
/// 启动延迟 30s：先等 sync 完成一轮（同步会 apply_server_defaults 等），
/// 避免启动瞬间与同步抢网络/文件。
///
/// 触发点设计（避免打断用户正在进行的操作）：仅当无镜像上传进行中时自动更新；
/// 失败一律仅日志 + 托盘可见状态，下次周期重试。
pub async fn spawn_self_update_loop<R: Runtime>(app: &AppHandle<R>) {
    // 启动延迟：先等 sync 完成一轮，避免启动即抢
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    const CHECK_INTERVAL: u64 = 6 * 60 * 60; // 6 小时

    loop {
        if let Err(e) = check_and_update(app).await {
            log::warn!("launcher 自更新检查失败（下次重试）：{e}");
        }
        // 刷新托盘：让「检查更新」项显示最新判定结果
        crate::tray::refresh_sync_menu(app);
        tokio::time::sleep(std::time::Duration::from_secs(CHECK_INTERVAL)).await;
    }
}

/// 单轮检查 + 更新（独立函数便于 CLI/测试复用）。
///
/// 检查阶段用 `check_only`（只判定、写缓存），发现新版才进入下载/替换。
/// **每一步都有可见反馈**：进度窗口的步骤 + 托盘状态 + 系统通知。
pub async fn check_and_update<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let (outcome, meta) = fetch_and_classify(app).await;
    match outcome {
        CheckOutcome::NoRelease { current } => {
            log::info!("launcher 自更新：服务端无新发布（当前 v{current}）");
            return Ok(());
        }
        CheckOutcome::Failed { error } => {
            log::warn!("launcher 自更新：检查失败 {error}");
            return Ok(()); // 失败不抛错：下次周期重试，不打扰用户
        }
        CheckOutcome::UpToDate { current } => {
            log::info!("launcher 自更新：已是最新（{current}）");
            return Ok(());
        }
        CheckOutcome::Available { version, notes, size, .. } => {
            // 镜像上传进行中不打断（文件操作与网络都被占用）
            let cfg = crate::config::load_cached();
            if crate::mirror::load_progress(app, &cfg).state == "running" {
                log::info!("launcher 自更新：镜像上传进行中，跳过本轮（下轮再试）");
                return Ok(());
            }
            log::info!(
                "发现 launcher 新版 v{}（当前 v{}，{} MB{}），开始下载…",
                version,
                current_version(),
                size / 1024 / 1024,
                if notes.is_empty() { String::new() } else { format!("，说明：{notes}") }
            );
            // 进度窗口：把「下载 → 校验 → 替换 → 重启」四步摊开给用户看
            crate::ops::start_op(
                app,
                "launcher-update",
                "更新 launcher",
                &["下载新版本", "校验完整性", "替换程序", "重启 launcher"],
            );
            crate::ops::set_details(
                app,
                &[format!(
                    "launcher {} → {version}{}",
                    current_version(),
                    if notes.is_empty() { String::new() } else { format!("（{notes}）") }
                )],
            );
            crate::ops::mark_step_running(app, 0);
            crate::ops::update_step(app, &format!("下载 v{version}（{} MB）…", size / 1024 / 1024));
            let notify_msg = format!("发现新版本 v{version}，正在自动更新…");
            crate::notify::notify(app, "DeepSeek Harness Launcher", &notify_msg);
            crate::tray::refresh_sync_menu(app);

            // 下载到 exe 同目录的临时名（同文件系统，rename 原子）
            let exe_dir = current_exe().parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
            let tmp = exe_dir.join(format!("launcher-{version}.new.exe"));
            // fetch_and_classify 已带回完整元数据（含 sha256/file）；理论上 Available
            // 必然伴随 Some(meta)，缺失时构造最小元数据兜底（下载仍需 file 字段）。
            let full_meta = meta.unwrap_or(ReleaseMeta {
                version: version.clone(),
                file: format!("launcher-{version}.exe"),
                sha256: String::new(),
                notes: notes.clone(),
                size,
                published_at: None,
                no_release: false,
            });
            if let Err(e) = download_release(app, &full_meta, &tmp).await {
                crate::ops::fail_op(app, &format!("下载失败：{e}"));
                crate::notify::notify(app, "launcher 更新失败", &format!("下载失败：{e}\n可稍后重试"));
                crate::tray::refresh_sync_menu(app);
                return Err(e);
            }
            crate::ops::mark_step_running(app, 1);
            crate::ops::update_step(app, "校验 sha256…");
            log::info!(
                "下载完成：v{} {} MB（发布 {}）",
                version,
                size / 1024 / 1024,
                full_meta.published_at.as_deref().unwrap_or("未知")
            );

            // 防呆：若发布物与本机 exe 内容相同（sha256 一致，如误发布同二进制），跳过更新，
            // 避免「换汤不换药」导致无限下载-替换循环
            if let Ok(cur_bytes) = std::fs::read(current_exe()) {
                let cur_sha = format!("{:x}", Sha256::digest(&cur_bytes));
                if !full_meta.sha256.is_empty() && cur_sha == full_meta.sha256.to_ascii_lowercase() {
                    log::warn!("发布物与本机 exe 内容相同（sha256 一致 v{version}），跳过无意义更新");
                    let _ = std::fs::remove_file(&tmp);
                    crate::ops::finish_op(app, "已是最新（发布物与本机相同，无需更新）");
                    crate::tray::refresh_sync_menu(app);
                    return Ok(());
                }
            }

            crate::ops::mark_step_running(app, 2);
            crate::ops::update_step(app, "替换程序并重启…");
            // 触发替换：spawn 新 exe 自举（--cmd update-self --update-file <新exe>），
            // 然后本进程退出（助手会把它 rename 成正式名并重启）
            if let Err(e) = spawn_update_self(app, &tmp) {
                crate::ops::fail_op(app, &format!("启动更新助手失败：{e}"));
                crate::notify::notify(app, "launcher 更新失败", &e);
                crate::tray::refresh_sync_menu(app);
                return Err(e);
            }
            crate::ops::mark_step_running(app, 3);
            crate::ops::update_step(app, "正在重启 launcher…");
            log::info!("launcher 自更新：助手已启动，本进程退出");
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            crate::tray::request_quit(app);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_newer_compares_versions() {
        // 当前版本来自编译期常量，此处测比较逻辑用相对断言
        let cur = current_version();
        // 比当前大的版本 → 需要更新
        assert!(has_newer(&format!("{}", bump_major(&cur))));
        // 同版本 → 不需要
        assert!(!has_newer(&cur));
        // 小版本 → 不需要
        assert!(!has_newer("0.0.1"));
        // v 前缀
        assert!(has_newer(&format!("v{}", bump_major(&cur))));
    }

    fn bump_major(v: &str) -> String {
        let parts: Vec<&str> = v.split('.').collect();
        let major: u64 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
        format!("{}.0.0", major + 1)
    }

    #[test]
    fn has_newer_tolerates_bad_remote() {
        // 远程非 semver → 保守视为需要更新
        assert!(has_newer("not-a-version"));
        assert!(has_newer(""));
    }

    #[test]
    fn check_outcome_labels_and_messages() {
        let up = CheckOutcome::UpToDate { current: "0.4.1".into() };
        assert!(up.menu_label().contains("已是最新"));
        assert!(up.message().contains("0.4.1"));
        assert!(!up.has_update(), "已是最新不应触发更新");

        let avail = CheckOutcome::Available {
            current: "0.4.1".into(),
            version: "0.4.2".into(),
            notes: "修更新提示".into(),
            size: 6 * 1024 * 1024,
        };
        assert!(avail.menu_label().contains("0.4.2"));
        assert!(avail.message().contains("修更新提示"));
        assert!(avail.message().contains("6 MB"));
        assert!(avail.has_update(), "发现新版必须触发更新");

        // 无发布 ≠ 已是最新（对用户含义不同：一个是「服务端没发过」，一个是「你已最新」）
        let none = CheckOutcome::NoRelease { current: "0.4.1".into() };
        assert!(none.menu_label().contains("服务端无发布"));
        assert!(!none.has_update());

        // 失败 ≠ 已是最新：绝不能把「查不到」显示成「已是最新」
        let failed = CheckOutcome::Failed { error: "连接超时".into() };
        assert!(failed.menu_label().contains("失败"));
        assert!(failed.message().contains("连接超时"));
        assert!(!failed.has_update());
        assert!(
            !failed.message().contains("已是最新"),
            "检查失败不得谎报已是最新"
        );
    }

    #[test]
    fn last_check_cache_roundtrip() {
        reset_last_check();
        assert!(last_check().is_none(), "初始应无缓存");

        remember(&CheckOutcome::UpToDate { current: "9.9.9".into() });
        let (o, at) = last_check().expect("写入后应能读到");
        assert_eq!(o, CheckOutcome::UpToDate { current: "9.9.9".into() });
        assert!(!at.is_empty(), "应带检查时间戳");
        reset_last_check();
    }
}
