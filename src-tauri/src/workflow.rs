//! Harness 服务生命周期：启动 / 停止 / 端口 / 进程树回收 / 存活状态。
//!
//! 启动命令：`node <dsh>/node_modules/@deepseek-ai/dsh/lib/bin.js --profile web
//! --host 127.0.0.1 --port <port>`（rc.8+ 追加 `--no-open`，避免弹浏览器）。
//! Windows 以 `CREATE_NO_WINDOW` 生成子进程，退出时 `taskkill /T /F` 回收整棵进程树，
//! 避免 DLL 锁影响后续更新。无 webview，因此启动后只记录 PID、由托盘控制停止。
//!
//! 状态：进程内记录 `(pid, port)`。`is_running` / `launch` 会校验 PID 是否存活，
//! 崩溃后自动清理状态，避免「端口已释放但托盘仍显示已运行」的误判。

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use tauri::{AppHandle, Runtime};

use crate::config::*;

/// 运行中的 Harness 服务（进程内唯一）。
struct Running {
    pid: u32,
    port: u16,
    profile: String,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

/// 最近一次成功启动的端口（供「打开 Harness 页面」）。
pub fn last_port() -> Option<u16> {
    RUNNING.lock().unwrap().as_ref().map(|r| r.port)
}

/// 当前运行中的 profile 名（未运行时返回 None）。
pub fn current_profile() -> Option<String> {
    let running = RUNNING.lock().unwrap();
    running.as_ref().filter(|r| pid_alive(r.pid)).map(|r| r.profile.clone())
}

/// Harness 是否在运行（记录过 PID 且该进程仍存活）。
pub fn is_running() -> bool {
    let running = RUNNING.lock().unwrap();
    match &*running {
        Some(r) => pid_alive(r.pid),
        None => false,
    }
}

/// 若记录的进程已退出，清理状态（崩溃自愈）。
fn reap_if_dead() {
    let mut running = RUNNING.lock().unwrap();
    if let Some(r) = running.as_ref() {
        if !pid_alive(r.pid) {
            log::warn!("记录的 Harness 进程 PID={} 已退出，清理状态", r.pid);
            *running = None;
        }
    }
}

/// 端口是否已被占用（bind 失败 = 占用；成功后临时 listener 立即释放）。
fn port_in_use(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_err()
}

fn find_available_port(start: u16) -> Result<u16, String> {
    let mut port = start;
    loop {
        if !port_in_use(port) {
            return Ok(port);
        }
        log::warn!("端口 {port} 被占用，尝试下一个");
        port = port.checked_add(1).ok_or("PORT_EXHAUSTED: 无可用端口")?;
    }
}

/// 组装 dsh 子进程需要的环境：DSH_HOME + PATH（node/pnpm/git）+ 加速源。
///
/// 供启动、插件安装、同步检查等所有 dsh 子进程共用，保证行为一致。
pub fn child_env<R: Runtime>(
    app: &AppHandle<R>,
    cfg: &LauncherConfig,
) -> Result<Vec<(String, String)>, String> {
    let mut env: Vec<(String, String)> = Vec::new();
    env.push(("DSH_HOME".to_string(), dsh_home(app, cfg).to_string_lossy().to_string()));

    let mut path = Vec::new();
    path.push(runtime_path(app).clone());
    #[cfg(windows)]
    path.push(git_install_path(app).join("cmd"));
    path.push(pnpm_install_path(app).join("bin"));
    if let Some(existing) = std::env::var_os("PATH") {
        path.push(PathBuf::from(existing));
    }
    let joined = std::env::join_paths(path).map_err(|e| e.to_string())?;
    env.push(("PATH".to_string(), joined.to_string_lossy().to_string()));

    // npm registry：仅当用户**显式配置** npmRegistry 时才注入环境变量（显式配置优先）。
    // 未配置时交由 profile 的 `.npmrc`（ensure_profile_npmrc 已按地域写入）生效，
    // 避免这里按地域解析出的值覆盖掉用户 profile 里手工设的 registry。
    if let Some(reg) = cfg.npm_registry.as_deref() {
        let reg = reg.trim();
        if !reg.is_empty() {
            env.push(("npm_config_registry".to_string(), reg.to_string()));
        }
    }
    if let Some(prefix) = resolve_gh_prefix(cfg) {
        env.push(("GIT_CONFIG_COUNT".to_string(), "1".to_string()));
        env.push(("GIT_CONFIG_KEY_0".to_string(), format!("url.{prefix}insteadOf")));
        env.push(("GIT_CONFIG_VALUE_0".to_string(), "https://github.com/".to_string()));
    }
    Ok(env)
}

fn apply_env(cmd: &mut Command, env: &[(String, String)]) {
    for (k, v) in env {
        cmd.env(k, v);
    }
}

/// 启动 Harness 服务（幂等：已在运行则返回现有端口）。使用配置的 profile。
pub fn launch<R: Runtime>(app: &AppHandle<R>) -> Result<u16, String> {
    let profile = load_cached().profile.clone().unwrap_or_else(|| "web".to_string());
    launch_with_profile(app, &profile)
}

/// 以指定 profile 启动 Harness 服务。
///
/// 幂等：已在运行且 profile 相同 → 返回现有端口；已在运行但 profile 不同 → 先停止再切换。
pub fn launch_with_profile<R: Runtime>(app: &AppHandle<R>, profile: &str) -> Result<u16, String> {
    reap_if_dead();

    // 已在运行：相同 profile 直接返回；不同 profile 先停止（切换）
    if let Some(r) = RUNNING.lock().unwrap().as_ref() {
        if pid_alive(r.pid) {
            if r.profile == profile {
                log::info!("Harness 已在运行：PID={}, 端口={}, profile={}", r.pid, r.port, r.profile);
                return Ok(r.port);
            }
            log::info!("切换 profile：{} -> {}（先停止当前）", r.profile, profile);
            stop();
        }
    }

    let cfg = load_cached();
    let port = find_available_port(resolve_port(&cfg))?;

    let node = node_binary_path(app);
    if !node.exists() {
        return Err("NODE_NOT_FOUND: 尚未安装 Node.js 运行时，请先「安装 / 修复」".to_string());
    }
    let dsh_bin = dsh_binary_path(app);
    if !dsh_bin.exists() {
        return Err("DSH_NOT_FOUND: 尚未安装 Harness 核心，请先「安装 / 修复」".to_string());
    }
    let cwd = dsh_install_path(app);

    let mut cmd = Command::new(&node);
    cmd.arg(&dsh_bin)
        .arg("--profile")
        .arg(profile)
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // rc.8+ 支持 --no-open（启动不弹系统浏览器）；更早版本无此标志。
    if version_supports_no_open(app) {
        cmd.arg("--no-open");
    }

    // 环境变量：DSH_HOME（隔离数据目录）+ PATH（让 dsh 内部调用 node/pnpm/git）
    // + 加速源（npm registry / git 中转）。
    apply_env(&mut cmd, &child_env(app, &cfg)?);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    let child = cmd.spawn().map_err(|e| format!("HARNESS_LAUNCH_FAILED: {e}"))?;
    let pid = child.id();
    // 不直接 wait，仅记录 PID + 端口；进程在后台运行。
    *RUNNING.lock().unwrap() = Some(Running { pid, port, profile: profile.to_string() });
    log::info!(
        "Harness 已启动：PID={}, 端口={}, profile={}, 数据目录={}",
        pid,
        port,
        profile,
        crate::config::dsh_home(app, &cfg).display()
    );
    Ok(port)
}

/// 停止 Harness 服务进程树。
pub fn stop() {
    let running = RUNNING.lock().unwrap().take();
    let Some(r) = running else {
        log::info!("没有需要停止的 Harness 进程");
        return;
    };
    kill_pid_tree(r.pid);
    log::info!("Harness 已停止：PID={}", r.pid);
}

fn kill_pid_tree(pid: u32) {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("taskkill");
        cmd.args(["/PID", &pid.to_string(), "/T", "/F"]);
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        if let Err(e) = cmd.output() {
            log::error!("停止 Harness 进程树 {pid} 失败：{e}");
        }
    }
    #[cfg(unix)]
    {
        let group = format!("-{pid}");
        let _ = Command::new("kill").args(["-TERM", "--", &group]).output();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let _ = Command::new("kill").args(["-KILL", "--", &group]).output();
    }
}

/// 进程是否仍存活。
///
/// Windows：`OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` + `GetExitCodeProcess`
/// 判 `STILL_ACTIVE`；Unix：`kill(pid, 0)` 探活。PID 复用属罕见竞态，可接受。
fn pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut code: u32 = 0;
            let ok = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            // STILL_ACTIVE 是 NTSTATUS(i32)，这里比较底层值 0x103
            ok != 0 && code == 0x103
        }
    }
    #[cfg(unix)]
    {
        // kill(pid, 0) 仅探活不发送信号；0 表示进程存在
        Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

/// 读取已装 dsh 版本（字符串，如 `0.1.1-rc.2`）；未安装/解析失败返回空串。
pub fn installed_dsh_version<R: Runtime>(app: &AppHandle<R>) -> Option<String> {
    let manifest = dsh_install_path(app).join("package.json");
    let text = std::fs::read_to_string(&manifest).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let version = json.get("version")?.as_str()?.to_string();
    Some(version)
}

/// 读取已装 dsh 版本，判断是否支持 `--no-open`（>= 0.1.0-rc.8）。
fn version_supports_no_open<R: Runtime>(app: &AppHandle<R>) -> bool {
    const MIN: &str = "0.1.0-rc.8";
    let Some(version) = installed_dsh_version(app) else {
        return false;
    };
    let (Ok(min), Ok(ver)) = (semver::Version::parse(MIN), semver::Version::parse(&version)) else {
        return false;
    };
    ver >= min
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_probe_detects_in_use() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(port_in_use(port), "已绑定端口应被判定为占用");
        drop(listener);
        // Windows 上端口释放存在短暂竞态（TIME_WAIT），重试几次
        let mut released = false;
        for _ in 0..20 {
            if !port_in_use(port) {
                released = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(released, "释放后端口应空闲（重试后仍占用）");
    }

    #[test]
    fn find_available_port_skips_occupied() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let occupied = listener.local_addr().unwrap().port();
        let found = find_available_port(occupied).unwrap();
        assert_ne!(found, occupied, "不应返回被占用的端口");
        assert!(!port_in_use(found), "返回的端口应空闲");
    }

    #[test]
    fn running_state_tracks_port() {
        // 初始无状态
        assert_eq!(last_port(), None);
        assert!(!is_running());
        // 模拟运行中（用当前测试进程自己的 PID，必定存活）
        let test_port = crate::config::DEFAULT_PORT;
        *RUNNING.lock().unwrap() = Some(Running { pid: std::process::id(), port: test_port, profile: "web".to_string() });
        assert_eq!(last_port(), Some(test_port));
        assert!(is_running());
        assert_eq!(current_profile(), Some("web".to_string()));
        // 停止后清空
        RUNNING.lock().unwrap().take();
        assert_eq!(last_port(), None);
        assert!(!is_running());
        assert_eq!(current_profile(), None);
    }
}
