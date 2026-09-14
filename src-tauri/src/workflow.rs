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
use std::time::Duration;
use tauri::{AppHandle, Runtime};

use crate::config::*;

/// 运行中的 Harness 服务（进程内唯一）。
struct Running {
    pid: u32,
    port: u16,
    profile: String,
    /// 浏览器访问令牌（`?token=` 查询参数值）。
    ///
    /// 为什么需要：dsh 0.1.2+ 给 Web UI 加了**进程启动令牌**校验——
    /// 首次访问必须带 `?token=<随机值>`（由 dsh 启动时打印，形如
    /// `dsh web: http://127.0.0.1:3197/?token=xxx`），校验通过后种下签名 cookie
    /// 并 302 到干净的 `/`。不带 token 直接访问根路径会拿到 **401**。
    ///
    /// 旧版 dsh（0.1.1-rc.x 及更早）没有该机制，此时为 `None`，URL 不带参数。
    token: Option<String>,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

/// 最近一次成功启动的端口（供「打开 Harness 页面」）。
pub fn last_port() -> Option<u16> {
    RUNNING.lock().unwrap().as_ref().map(|r| r.port)
}

/// 可访问的 Harness Web URL（**带 token**，可直接在浏览器打开）。
///
/// 修复 2026-09-14：此前各处都拼 `http://127.0.0.1:{port}`（干净 URL），
/// 在 dsh 0.1.2+ 上会因缺 `?token=` 而 **401 打不开**——用户看到的就是
/// 「浏览器地址后面有一个 token，而我们系统里没加这个参数，所以打不开」。
///
/// 有 token 时拼 `http://127.0.0.1:{port}/?token=xxx`（与 dsh 自己
/// `authenticatedUrl()` 的产出一致：pathname 固定 `/`，token 作为唯一查询参数）。
pub fn access_url(port: u16) -> String {
    let token = RUNNING
        .lock()
        .unwrap()
        .as_ref()
        .filter(|r| r.port == port)
        .and_then(|r| r.token.clone());
    match token {
        Some(t) if !t.is_empty() => format!("http://127.0.0.1:{port}/?token={t}"),
        _ => format!("http://127.0.0.1:{port}"),
    }
}

/// 当前运行实例的可访问 URL（未运行时返回 None）。
pub fn current_access_url() -> Option<String> {
    last_port().map(access_url)
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
pub fn port_in_use(port: u16) -> bool {
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
    // 多源时用第一个（npm 环境变量只认一个 registry）。
    if let Some(list) = &cfg.npm_registry {
        if let Some(first) = list.iter().map(|s| s.trim()).find(|s| !s.is_empty()) {
            env.push(("npm_config_registry".to_string(), first.to_string()));
        }
    }
    // GitHub 中转：多源时 git 的 url.insteadOf 只认一个，取第一个
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

    let node = effective_node_path(app, &cfg);
    if !node.exists() {
        return Err("NODE_NOT_FOUND: 尚未安装 Node.js 运行时，请先「安装 / 修复」".to_string());
    }
    let dsh_bin = dsh_binary_path(app);
    if !dsh_bin.exists() {
        return Err("DSH_NOT_FOUND: 尚未安装 Harness 核心，请先「安装 / 修复」".to_string());
    }
    let cwd = dsh_install_path(app);

    // stdout + stderr 都重定向到同一个日志文件：dsh 的启动日志/错误大多走
    // stdout（Node console.log），此前 stdout 被 Stdio::null() 丢弃 → 日志文件 0 字节、
    // 「端口未就绪」查不到根因（同事实测踩坑）。现在两个流都落盘，便于诊断与收集。
    let cfg2 = load_cached();
    let launch_log = crate::config::dsh_home(app, &cfg2)
        .join("logs")
        .join(format!("dsh-launch-{}.log", std::process::id()));
    if let Some(parent) = launch_log.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let stdout_file = std::fs::File::create(&launch_log)
        .map_err(|e| format!("STDOUT_LOG_CREATE_FAILED: {e}"))?;
    let stderr_file = stdout_file
        .try_clone()
        .map_err(|e| format!("STDERR_LOG_CLONE_FAILED: {e}"))?;

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
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    log::info!("Harness 启动日志（stdout+stderr）：{}", launch_log.display());

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
    // 等待端口就绪：spawn 成功 ≠ 服务可用（dsh 冷启动需加载插件/起 HTTP）。
    // 轮询端口监听，就绪才算真正启动成功；超时/进程退出则报失败并清理。
    *RUNNING.lock().unwrap() = Some(Running {
        pid,
        port,
        profile: profile.to_string(),
        token: None, // 端口就绪后从日志里补
    });
    log::info!(
        "Harness 进程已 spawn：PID={}, 端口={}, profile={}，等待端口就绪…",
        pid,
        port,
        profile
    );
    let ready = wait_for_port(port, pid, Duration::from_secs(LAUNCH_READY_TIMEOUT_SECS));
    if !ready {
        // 启动失败：先判断进程是否已自行退出（区分「崩溃」与「卡住未监听」），
        // 再杀进程树 + 清理状态。带上日志路径，便于用户/管理员直接查看。
        let exited = !pid_alive(pid);
        kill_pid_tree(pid);
        *RUNNING.lock().unwrap() = None;
        return Err(format!(
            "HARNESS_NOT_READY: {LAUNCH_READY_TIMEOUT_SECS}s 内端口 {port} 未就绪（{}）\n启动日志：{}",
            if exited { "进程已退出" } else { "进程仍在运行但未监听端口" },
            launch_log.display()
        ));
    }

    // 抓取浏览器访问 token（dsh 0.1.2+ 必需，见 Running.token 的说明）。
    //
    // 时机：dsh 的 `dsh web: …?token=…` 是在**端口 bind 之后**打印的
    // （announceReady 在 loader 就绪回调里），所以端口就绪后再读日志。
    // 但两者几乎同时发生，日志刷盘可能有毫秒级延迟 → 小步重试几次。
    // 旧版 dsh 没有这一行，重试耗尽后保持 None（URL 不带参数，行为不变）。
    let token = read_launch_token(&launch_log, Duration::from_secs(5));
    if let Some(t) = &token {
        log::info!("已获取浏览器访问 token（长度 {}），打开页面时会自动带上", t.len());
    } else {
        log::info!("启动日志中无 token（旧版 dsh 无该机制），按干净 URL 处理");
    }
    if let Some(r) = RUNNING.lock().unwrap().as_mut() {
        if r.pid == pid {
            r.token = token;
        }
    }

    log::info!(
        "Harness 已启动：PID={}, 端口={}, profile={}, 数据目录={}",
        pid,
        port,
        profile,
        crate::config::dsh_home(app, &cfg).display()
    );
    Ok(port)
}

/// 启动后等待端口就绪的最大时长（秒）。
/// dsh 冷启动（插件加载 + HTTP 起服务）一般 5-20s，但首次安装后插件多、
/// 磁盘冷、企业安全软件扫描时可能显著更久——留足 90s 避免误报失败
/// （同事实测 40s 不够；超时后仍会 kill 并报错，不会假成功）。
const LAUNCH_READY_TIMEOUT_SECS: u64 = 90;

/// 从 dsh 启动日志里提取浏览器访问 token。
///
/// dsh 0.1.2+ 启动时会往 stdout 打印一行（见 `dsh-web-app` 的 `announceReady`）：
///
/// ```text
/// dsh web: http://127.0.0.1:3197/?token=xJJIaaYvmVJDnOMAO5h8IZvvYfXa-xY0wI8cVignoQQ
/// dsh web: http://127.0.0.1:3197/?token=xxx (LAN: http://10.0.0.5:3197/?token=xxx)
/// ```
///
/// 本函数从日志文本里找出第一个 `?token=` 的值。取不到（旧版 dsh 无该行）
/// 返回 `None` —— 此时 URL 不带参数，行为与旧版一致。
///
/// 注意：**不能**只认 `dsh web:` 前缀。dsh 的这行是 console.log 输出，
/// 经 launcher 重定向后可能与其他输出交错；按 `?token=` 直接扫更稳。
fn extract_token_from_log(text: &str) -> Option<String> {
    const MARKER: &str = "?token=";
    let idx = text.find(MARKER)?;
    let rest = &text[idx + MARKER.len()..];
    // token 是 base64url（A-Za-z0-9-_），取到第一个非该字符集为止
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(rest.len());
    let token = &rest[..end];
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

/// 从启动日志文件里读取浏览器访问 token（带小步重试）。
///
/// dsh 打印该行的时机紧跟在端口 bind 之后，但日志写盘可能有毫秒级延迟，
/// 因此重试若干次；拿不到就返回 None（旧版 dsh 没有这一行）。
fn read_launch_token(log_path: &std::path::Path, budget: Duration) -> Option<String> {
    let deadline = std::time::Instant::now() + budget;
    loop {
        if let Ok(text) = std::fs::read_to_string(log_path) {
            if let Some(t) = extract_token_from_log(&text) {
                return Some(t);
            }
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// 轮询等待端口被监听；同时检测进程是否提前退出。
fn wait_for_port(port: u16, pid: u32, timeout: Duration) -> bool {
    let started = std::time::Instant::now();
    let deadline = started + timeout;
    loop {
        if port_in_use(port) {
            log::info!("端口 {port} 已就绪（耗时 {:.1}s）", started.elapsed().as_secs_f64());
            return true;
        }
        if !pid_alive(pid) {
            log::warn!("Harness 进程 PID={pid} 已退出，端口 {port} 未就绪");
            return false;
        }
        if std::time::Instant::now() >= deadline {
            log::warn!("等待端口 {port} 就绪超时（{}s）", timeout.as_secs());
            return false;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// 停止 Harness 服务进程树。
/// 优先停本进程记录的子进程；若本进程无记录（如 CLI 模式、别的实例启动的），
/// 按配置端口探测占用进程并杀其进程树——保证切换 dsh 版本等操作能释放文件锁。
pub fn stop() {
    let running = RUNNING.lock().unwrap().take();
    match running {
        Some(r) => {
            kill_pid_tree(r.pid);
            log::info!("Harness 已停止：PID={}", r.pid);
        }
        None => {
            // 无记录：按端口探测（Windows netstat 找监听 PID）
            let cfg = load_cached();
            let port = resolve_port(&cfg);
            if let Some(pid) = port_listener_pid(port) {
                log::warn!("发现端口 {port} 被 PID={pid} 监听（非本进程记录），停止其进程树");
                kill_pid_tree(pid);
                log::info!("Harness 已停止（按端口探测）：PID={pid}");
            } else {
                log::info!("没有需要停止的 Harness 进程");
            }
        }
    }
}

/// 通过 netstat 查找监听指定端口的进程 PID（Windows/Unix 通用）。
fn port_listener_pid(port: u16) -> Option<u32> {
    let output = Command::new("netstat")
        .args(["-ano"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let needle = format!(":{port}");
    for line in text.lines() {
        if !line.contains("LISTENING") || !line.contains(&needle) {
            continue;
        }
        // 取行尾的 PID（netstat -ano 最后一列）
        if let Some(pid_str) = line.split_whitespace().last() {
            if let Ok(pid) = pid_str.parse::<u32>() {
                if pid != 0 {
                    return Some(pid);
                }
            }
        }
    }
    None
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

/// 读取已装 dsh 版本（字符串，如 `0.1.1-rc.2`）；未安装/解析失败返回 `None`。
///
/// ⚠️ 2026-09-14 修复：此前读的是 `dependencies/dsh/package.json`——
/// 那是 launcher 自己生成的**包装清单**（`{"name":"dsh-runtime","private":true,
/// "dependencies":{...}}`），**没有 `version` 字段** → 恒返回 None。
/// 后果：① 日志「当前 ，发现新版本 …」版本号为空；
/// ② `version_supports_no_open` 恒为 false → 启动不带 `--no-open` → 每次启动都弹浏览器。
///
/// 真实版本在 `node_modules/@deepseek-ai/dsh/package.json`。
/// 兼容旧结构：包装清单若确实带 `version`（历史版本）也认。
pub fn installed_dsh_version<R: Runtime>(app: &AppHandle<R>) -> Option<String> {
    let root = dsh_install_path(app);
    // 首选：真实包清单（npm 安装结构）
    let real = root
        .join("node_modules")
        .join(crate::dsh_npm::DSH_NPM_PACKAGE)
        .join("package.json");
    if let Some(v) = read_version_field(&real) {
        return Some(v);
    }
    // 兜底：包装清单（仅当它确实带 version 时）
    read_version_field(&root.join("package.json"))
}

/// 读某个 package.json 的 `version` 字段；文件不存在/无该字段返回 None。
fn read_version_field(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    json.get("version")?.as_str().map(|s| s.to_string())
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

    // ---------- 浏览器访问 token（dsh 0.1.2+ 必需） ----------

    #[test]
    fn extracts_token_from_real_dsh_line() {
        // 实测格式（dsh 0.1.2-rc.1 真实输出）
        let line = "dsh web: http://127.0.0.1:3197/?token=xJJIaaYvmVJDnOMAO5h8IZvvYfXa-xY0wI8cVignoQQ\n";
        assert_eq!(
            extract_token_from_log(line).as_deref(),
            Some("xJJIaaYvmVJDnOMAO5h8IZvvYfXa-xY0wI8cVignoQQ")
        );
    }

    #[test]
    fn extracts_first_token_when_lan_address_present() {
        // 有 LAN 时 dsh 会打印两个 token；取第一个（本机回环）
        let line = "dsh web: http://127.0.0.1:3197/?token=AAAA1111 (LAN: http://10.0.0.5:3197/?token=BBBB2222)\n";
        assert_eq!(extract_token_from_log(line).as_deref(), Some("AAAA1111"));
    }

    #[test]
    fn extracts_token_from_noisy_log() {
        // 真实日志里该行与其它输出交错
        let log = "[INFO] loading plugins...\n[INFO] web server listening\n\
                   dsh web: http://127.0.0.1:3180/?token=abc-DEF_123\n[INFO] done\n";
        assert_eq!(extract_token_from_log(log).as_deref(), Some("abc-DEF_123"));
    }

    #[test]
    fn returns_none_for_legacy_dsh_without_token() {
        // 旧版 dsh（0.1.1-rc.2）只打印干净 URL
        let line = "dsh web: http://127.0.0.1:3180\n";
        assert_eq!(extract_token_from_log(line), None);
        assert_eq!(extract_token_from_log(""), None);
        assert_eq!(extract_token_from_log("no url here"), None);
    }

    #[test]
    fn ignores_empty_token_value() {
        assert_eq!(extract_token_from_log("?token=\n"), None);
        assert_eq!(extract_token_from_log("?token= (LAN: x)\n"), None);
    }

    #[test]
    fn access_url_includes_token_when_present() {
        // 无运行实例 → 干净 URL（保持旧行为）
        assert_eq!(access_url(3180), "http://127.0.0.1:3180");
    }

    #[test]
    fn access_url_uses_token_for_matching_port() {
        *RUNNING.lock().unwrap() = Some(Running {
            pid: 0,
            port: 3199,
            profile: "web".to_string(),
            token: Some("tok-abc".to_string()),
        });
        assert_eq!(access_url(3199), "http://127.0.0.1:3199/?token=tok-abc");
        // 端口不匹配时不套用（避免把 A 实例的 token 用到 B 实例）
        assert_eq!(access_url(3180), "http://127.0.0.1:3180");
        *RUNNING.lock().unwrap() = None;
    }

    #[test]
    fn access_url_clean_when_token_absent() {
        *RUNNING.lock().unwrap() = Some(Running {
            pid: 0,
            port: 3198,
            profile: "web".to_string(),
            token: None,
        });
        assert_eq!(access_url(3198), "http://127.0.0.1:3198");
        *RUNNING.lock().unwrap() = None;
    }

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
        *RUNNING.lock().unwrap() = Some(Running {
            pid: std::process::id(),
            port: test_port,
            profile: "web".to_string(),
            token: None,
        });
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
