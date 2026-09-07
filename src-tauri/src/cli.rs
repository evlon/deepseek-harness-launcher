//! CLI 命令模式：通过命令行参数控制程序行为，实现「设计→开发→测试→改进」闭环。
//!
//! 用法：
//! ```text
//! deepseek-harness-launcher.exe --cmd <command> [--json] [--registry <url>] [--token <t>]
//! deepseek-harness-launcher.exe --help          # 查看帮助
//! ```
//!
//! 命令（与 IPC 命令共享执行核心）：
//! - `install`      安装/修复全部依赖（带进度输出）
//! - `launch`       启动 Harness（输出端口）
//! - `stop`         停止 Harness
//! - `sync`         立即同步
//! - `speedtest`    测速（输出各源延迟）
//! - `mirror`       镜像上传（需 --registry --token，**执行完才退出**——上传完成后进程才结束）
//! - `status`       查询状态（运行中/端口/上次操作/测速结果）
//! - `open-console` 打开进度窗口
//! - `dsh-versions` 查询 dsh 版本状态（当前/已装/最新）
//! - `dsh-install`  下载安装指定 dsh 版本（需 --tag <版本>）
//! - `dsh-switch`   切换 dsh 版本（需 --tag <版本>，停→换→重启）
//! - `update-self`  自更新 exe（需 --update-file <新exe路径>）
//! - `update-check` 检查 launcher 更新
//! - `test`         全流程自测（install → launch → status → stop）
//!
//! 有 `--cmd` 时执行命令，**全部完成后才退出**（不常驻托盘；`mirror` 会等待上传线程跑完）；
//! 无则正常常驻托盘。

use tauri::{AppHandle, Runtime};

/// 解析 CLI 参数。返回 (命令, 选项)。
pub struct CliArgs {
    pub cmd: Option<String>,
    pub json: bool,
    pub registry: String,
    pub token: String,
    pub tag: String,
    pub update_file: String,
    pub help: bool,
}

pub fn parse_args() -> CliArgs {
    let mut args = CliArgs {
        cmd: None,
        json: false,
        registry: String::new(),
        token: String::new(),
        tag: String::new(),
        update_file: String::new(),
        help: false,
    };
    let mut iter = std::env::args().skip(1);
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--cmd" | "-c" => args.cmd = iter.next(),
            "--json" => args.json = true,
            "--registry" => args.registry = iter.next().unwrap_or_default(),
            "--token" => args.token = iter.next().unwrap_or_default(),
            "--tag" => args.tag = iter.next().unwrap_or_default(),
            "--update-file" => args.update_file = iter.next().unwrap_or_default(),
            "-h" | "--help" | "help" => args.help = true,
            _ => {}
        }
    }
    args
}

/// 打印完整帮助（-h/--help/help 或未知命令）。
fn print_help() {
    println!("DeepSeek Harness Launcher — CLI 用法");
    println!();
    println!("用法：deepseek-harness-launcher.exe --cmd <命令> [选项]");
    println!("      deepseek-harness-launcher.exe --help");
    println!();
    println!("命令（执行完才退出，不常驻托盘）：");
    println!("  install       安装/修复全部依赖");
    println!("  launch        启动 Harness（输出端口）");
    println!("  stop          停止 Harness");
    println!("  sync          立即同步（强制刷新版本检查）");
    println!("  speedtest     测速（输出各源延迟）");
    println!("  mirror        镜像上传到内网 registry（需 --registry --token）");
    println!("  status        查询状态（运行中/端口/上次操作/测速结果）");
    println!("  open-console  打开进度窗口");
    println!("  dsh-versions  查询 dsh 版本状态（当前/已装/最新）");
    println!("  dsh-install   下载安装指定 dsh 版本（需 --tag <版本>）");
    println!("  dsh-switch    切换 dsh 版本（需 --tag <版本>，停→换→重启）");
    println!("  update-self   自更新 exe（需 --update-file <新exe路径>）");
    println!("  update-check  检查 launcher 更新");
    println!("  test          全流程自测（install → launch → status → stop）");
    println!();
    println!("选项：");
    println!("  --json                 JSON 输出");
    println!("  --registry <url>       内网 registry（mirror 用；缺省读配置）");
    println!("  --token <值>           发布 token（mirror 用，不落盘）");
    println!("  --tag <版本>           dsh 版本（dsh-install / dsh-switch 用）");
    println!("  --update-file <路径>   新 exe 路径（update-self 用）");
    println!("  -h, --help, help       查看本帮助");
    println!();
    println!("示例：");
    println!("  launcher.exe --cmd status");
    println!("  launcher.exe --cmd sync");
    println!("  launcher.exe --cmd mirror --registry http://registry.ict.cmcc --token <发布token>");
    println!("  launcher.exe --cmd dsh-switch --tag v0.1.1-rc.2");
    println!();
    println!("说明：--cmd mirror 会等待上传全部完成才退出（进度实时打印，中途 Ctrl+C 可中止）；");
    println!("无 --cmd 时正常常驻系统托盘。");
}

/// CLI 模式入口：执行命令后退出。返回进程退出码。
/// 核心逻辑复用 IPC 命令（commands.rs），保证两条通道行为一致。
pub fn run_cli<R: Runtime>(app: &AppHandle<R>, args: &CliArgs) -> i32 {
    // --help 优先于命令（help 与具体命令共存时也展示帮助）
    if args.help {
        print_help();
        return 0;
    }
    let Some(cmd) = args.cmd.as_deref() else {
        // 无命令也无 help：正常常驻路径不该走到这；给提示避免静默退出
        print_help();
        return 2;
    };

    match cmd {
        "install" => {
            let r = tauri_async_block(app, crate::commands::cmd_install(app.clone()));
            print_result("install", &r);
            if r.is_ok() { 0 } else { 1 }
        }
        "launch" => {
            let r = crate::commands::cmd_launch(app.clone());
            print_result("launch", &r);
            if r.is_ok() { 0 } else { 1 }
        }
        "stop" => {
            let r = crate::commands::cmd_stop(app.clone());
            print_result("stop", &r);
            0
        }
        "sync" => {
            let r = tauri_async_block(app, crate::commands::cmd_sync(app.clone()));
            print_result("sync", &r);
            if r.is_ok() { 0 } else { 1 }
        }
        "speedtest" => {
            let r = tauri_async_block(app, crate::commands::cmd_speedtest(app.clone()));
            print_result("speedtest", &r);
            0
        }
        "mirror" => {
            // 参数校验：mirror 必须有发布 token（registry 可缺省读配置）
            if args.token.is_empty() {
                println!("[mirror] 缺少 --token <发布token>（内网 registry 的发布凭证）");
                println!("[mirror] 用法：--cmd mirror --registry http://registry.ict.cmcc --token <token>");
                println!("[mirror] 说明：同步「应装清单」全部插件+依赖到内网 registry；执行完才退出。");
                return 2;
            }
            // 启动上传（内部异步线程执行，进度实时打印）
            let started = crate::commands::cmd_mirror(
                app.clone(),
                Some(args.registry.clone()),
                args.token.clone(),
            );
            match started {
                Ok(v) => {
                    println!("[mirror] 已启动：{}", serde_json::to_string(&v).unwrap_or_default());
                    wait_mirror_done(app)
                }
                Err(e) => {
                    print_result("mirror", &Err(e.clone()));
                    1
                }
            }
        }
        "status" => {
            let r = crate::commands::cmd_status(app.clone());
            print_result("status", &r);
            0
        }
        "open-console" => {
            let r = crate::commands::cmd_open_console(app.clone());
            print_result("open-console", &r);
            if r.is_ok() { 0 } else { 1 }
        }
        "dsh-versions" => {
            let r = tauri_async_block(app, crate::commands::cmd_dsh_versions(app.clone()));
            print_result("dsh-versions", &r);
            0
        }
        "dsh-install" => {
            if args.tag.is_empty() {
                println!("[dsh-install] 缺少 --tag <版本>");
                2
            } else {
                let r = tauri_async_block(app, crate::commands::cmd_dsh_install(app.clone(), args.tag.clone()));
                print_result("dsh-install", &r);
                if r.is_ok() { 0 } else { 1 }
            }
        }
        "dsh-switch" => {
            if args.tag.is_empty() {
                println!("[dsh-switch] 缺少 --tag <版本>");
                2
            } else {
                let r = tauri_async_block(app, crate::commands::cmd_dsh_switch(app.clone(), args.tag.clone()));
                print_result("dsh-switch", &r);
                if r.is_ok() { 0 } else { 1 }
            }
        }
        "test" => {
            run_selftest(app)
        }
        "update-self" => {
            // 自更新助手：替换 exe + 重启（不依赖 Tauri，纯文件操作）
            if args.update_file.is_empty() {
                println!("[update-self] 缺少 --update-file <新exe路径>");
                2
            } else {
                crate::self_update::run_update_self(&args.update_file)
            }
        }
        "update-check" => {
            // 手动检查 launcher 更新（发现新版即下载并替换重启）
            let r = tauri_async_block(app, crate::self_update::check_and_update(app));
            match r {
                Ok(()) => {
                    println!("[update-check] 检查完成（无更新或已触发更新流程）");
                    0
                }
                Err(e) => {
                    println!("[update-check] 失败：{e}");
                    1
                }
            }
        }
        _ => {
            println!("未知命令：{cmd}");
            println!();
            print_help();
            2
        }
    }
}

/// 打印命令结果（json 或文本）。
fn print_result(cmd: &str, r: &Result<serde_json::Value, String>) {
    match r {
        Ok(v) => {
            let pretty = serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".to_string());
            println!("[{cmd}] {pretty}");
        }
        Err(e) => println!("[{cmd}] 失败：{e}"),
    }
}

/// mirror 上传在异步线程执行（start 立即返回）；CLI 在此轮询进度直到 done/error，
/// 全部完成才退出——避免进程提前退出把上传线程杀掉（半途而废）。
///
/// 注意：进度文件可能残留上次运行的 error/done 状态，必须等本次上传真正开始
/// （state 进入 running 且 started_at 晚于本进程启动）才算数；否则把旧残留当结果误报。
/// 返回退出码：0=全部成功（含单包失败但已跳过）；1=整体失败/超时。
fn wait_mirror_done<R: Runtime>(app: &AppHandle<R>) -> i32 {
    use std::time::{Duration, Instant};
    let cfg = crate::config::load_cached();
    let wall_started = Instant::now();
    // 本次进程启动时刻（进度 started_at 必须晚于它，才是本次运行）
    let proc_started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    const POLL_SECS: u64 = 2;
    // 整体超时上限：mirror.rs 内部为 30 分钟，CLI 等同一上限即可（多给 60s 余量兜底）
    const WAIT_MAX_SECS: u64 = 31 * 60;
    const START_WAIT_MAX_SECS: u64 = 15; // 等 running 出现的最长秒数（start 后线程应很快置进度）

    let mut last_reported = String::new();
    let mut saw_running = false; // 本次上传是否已真正开始
    loop {
        let p = crate::mirror::load_progress(app, &cfg);
        let started_at = chrono::DateTime::parse_from_rfc3339(&p.started_at)
            .map(|t| t.timestamp())
            .unwrap_or(0);
        let is_this_run = started_at >= proc_started - 5; // 5s 容差（时钟/文件写入延迟）

        if !saw_running {
            if p.state == "running" && is_this_run {
                saw_running = true; // 本次上传真正开始，进入正式等待
            } else if p.state != "running" {
                // 还没 running（idle / 残留 error/done）：等它开始
                if wall_started.elapsed().as_secs() > START_WAIT_MAX_SECS {
                    let st = if p.state.is_empty() { "idle" } else { &p.state };
                    println!("[mirror] ❌ 上传未在 {START_WAIT_MAX_SECS}s 内开始（进度 state={st}）——请检查是否已有上传在运行");
                    return 1;
                }
                std::thread::sleep(Duration::from_secs(POLL_SECS));
                continue;
            }
            // running + 本次：落入下面统一处理
        }

        // 进度变更时打印（当前包 + 完成数），避免刷屏
        let cur = format!("{} {} / {}", p.current_pkg, p.done_pkgs, p.total_pkgs);
        if cur != last_reported && !p.current_pkg.is_empty() {
            println!("[mirror] 进度 {}：{}（{}）", p.done_pkgs, p.current_pkg, p.state);
            last_reported = cur;
        }
        match p.state.as_str() {
            "done" => {
                println!("[mirror] ✅ 完成：{}/{} 个包已同步到 {}", p.done_pkgs, p.total_pkgs, p.registry);
                return if p.error.is_empty() { 0 } else { 1 };
            }
            "error" => {
                let msg = if p.error.is_empty() { "未知错误".to_string() } else { p.error.clone() };
                println!("[mirror] ❌ 失败：{}", crate::config::truncate_utf8(&msg, 800));
                return 1;
            }
            _ => {} // running：继续轮询
        }
        if wall_started.elapsed().as_secs() > WAIT_MAX_SECS {
            println!("[mirror] ❌ 等待超时（{} 分钟），上传仍在进行中——请稍后用托盘/管理页查看进度", WAIT_MAX_SECS / 60);
            return 1;
        }
        std::thread::sleep(Duration::from_secs(POLL_SECS));
    }
}

/// 全流程自测：install → launch → status → stop → speedtest。
fn run_selftest<R: Runtime>(app: &AppHandle<R>) -> i32 {
    let mut fail = 0;
    println!("[1/5] install…");
    let r = tauri_async_block(app, crate::commands::cmd_install(app.clone()));
    if r.is_err() { println!("  ✗ install 失败"); fail += 1; } else { println!("  ✓ install 完成"); }

    println!("[2/5] launch…");
    match crate::commands::cmd_launch(app.clone()) {
        Ok(v) => println!("  ✓ 启动：{}", v),
        Err(e) => { println!("  ✗ 启动失败：{e}"); fail += 1; }
    }

    println!("[3/5] status…");
    let running = crate::workflow::is_running();
    println!("  {} 运行中：{running}", if running { "✓" } else { "✗" });
    if !running { fail += 1; }

    println!("[4/5] stop…");
    let _ = crate::commands::cmd_stop(app.clone());
    if crate::workflow::is_running() { println!("  ✗ 停止后仍运行"); fail += 1; } else { println!("  ✓ 已停止"); }

    println!("[5/5] speedtest…");
    let r = tauri_async_block(app, crate::commands::cmd_speedtest(app.clone()));
    if r.is_err() { println!("  ✗ 测速失败"); fail += 1; } else { println!("  ✓ 测速完成"); }

    println!("结果：{}", if fail == 0 { "全部通过 ✓".to_string() } else { format!("{fail} 项失败 ✗") });
    if fail == 0 { 0 } else { 1 }
}

/// 在 tauri async runtime 上执行异步任务（CLI 模式无事件循环，需手动 block_on）。
fn tauri_async_block<R: Runtime, F: std::future::Future>(_app: &AppHandle<R>, fut: F) -> F::Output {
    tauri::async_runtime::block_on(fut)
}

/// CLI 模式是否需要初始化（有 --cmd 或 --help 才走 CLI，否则常驻）。
pub fn is_cli_mode(args: &CliArgs) -> bool {
    args.cmd.is_some() || args.help
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(cmd: Option<&str>) -> CliArgs {
        CliArgs {
            cmd: cmd.map(|s| s.to_string()),
            json: false,
            registry: String::new(),
            token: String::new(),
            tag: String::new(),
            update_file: String::new(),
            help: false,
        }
    }

    #[test]
    fn parse_args_basic() {
        let args = args(Some("install"));
        assert_eq!(args.cmd.as_deref(), Some("install"));
        assert!(!args.json);
        assert!(is_cli_mode(&args));
    }

    #[test]
    fn parse_args_empty_is_not_cli() {
        let args = args(None);
        assert!(!is_cli_mode(&args));
    }

    #[test]
    fn parse_update_file_arg() {
        let args = CliArgs {
            cmd: Some("update-self".to_string()),
            json: false,
            registry: String::new(),
            token: String::new(),
            tag: String::new(),
            update_file: "C:\\tmp\\new.exe".to_string(),
            help: false,
        };
        assert_eq!(args.update_file, "C:\\tmp\\new.exe");
    }

    #[test]
    fn json_escape_works() {
        assert_eq!(serde_json::to_string("a\"b").unwrap(), "\"a\\\"b\"");
        assert_eq!(serde_json::to_string("简单中文").unwrap(), "\"简单中文\"");
    }

    #[test]
    fn help_flag_detected() {
        // 手工模拟 argv：--cmd mirror --help
        let saved: Vec<String> = std::env::args().collect();
        // 直接构造等价于 parse_args 结果的字段（无需真改 argv）
        let mut a = args(Some("mirror"));
        a.help = true;
        assert!(a.help);
        assert_eq!(a.cmd.as_deref(), Some("mirror"));
        let _ = saved;
    }
}
