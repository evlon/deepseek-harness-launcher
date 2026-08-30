//! CLI 命令模式：通过命令行参数控制程序行为，实现「设计→开发→测试→改进」闭环。
//!
//! 用法：
//! ```text
//! deepseek-harness-launcher.exe --cmd <command> [--json] [--registry <url>] [--token <t>]
//! ```
//!
//! 命令（与 IPC 命令共享执行核心）：
//! - `install`      安装/修复全部依赖（带进度输出）
//! - `launch`       启动 Harness（输出端口）
//! - `stop`         停止 Harness
//! - `sync`         立即同步
//! - `speedtest`    测速（输出各源延迟）
//! - `mirror`       镜像上传（需 --registry --token）
//! - `status`       查询状态（运行中/端口/上次操作/测速结果）
//! - `open-console` 打开进度窗口
//! - `dsh-versions` 查询 dsh 版本状态（当前/已装/最新）
//! - `dsh-install`  下载安装指定 dsh 版本（需 --tag <版本>）
//! - `dsh-switch`   切换 dsh 版本（需 --tag <版本>，停→换→重启）
//! - `test`         全流程自测（install → launch → status → stop）
//!
//! 有 `--cmd` 时执行命令后退出（不常驻托盘）；无则正常常驻。

use tauri::{AppHandle, Runtime};

/// 解析 CLI 参数。返回 (命令, 选项)。
pub struct CliArgs {
    pub cmd: Option<String>,
    pub json: bool,
    pub registry: String,
    pub token: String,
    pub tag: String,
}

pub fn parse_args() -> CliArgs {
    let mut args = CliArgs {
        cmd: None,
        json: false,
        registry: String::new(),
        token: String::new(),
        tag: String::new(),
    };
    let mut iter = std::env::args().skip(1);
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--cmd" | "-c" => args.cmd = iter.next(),
            "--json" => args.json = true,
            "--registry" => args.registry = iter.next().unwrap_or_default(),
            "--token" => args.token = iter.next().unwrap_or_default(),
            "--tag" => args.tag = iter.next().unwrap_or_default(),
            _ => {}
        }
    }
    args
}

/// CLI 模式入口：执行命令后退出。返回进程退出码。
/// 核心逻辑复用 IPC 命令（commands.rs），保证两条通道行为一致。
pub fn run_cli<R: Runtime>(app: &AppHandle<R>, args: &CliArgs) -> i32 {
    let Some(cmd) = args.cmd.as_deref() else {
        return 0; // 无命令 → 正常常驻（调用方处理）
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
            let r = crate::commands::cmd_mirror(
                app.clone(),
                Some(args.registry.clone()),
                args.token.clone(),
            );
            print_result("mirror", &r);
            if r.is_ok() { 0 } else { 1 }
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
        _ => {
            println!("未知命令：{cmd}");
            println!("可用命令：install / launch / stop / sync / speedtest / mirror / status / open-console / dsh-versions / dsh-install(--tag) / dsh-switch(--tag) / test");
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

/// CLI 模式是否需要初始化（有 --cmd 才走 CLI，否则常驻）。
pub fn is_cli_mode(args: &CliArgs) -> bool {
    args.cmd.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_basic() {
        let args = CliArgs {
            cmd: Some("install".to_string()),
            json: true,
            registry: String::new(),
            token: String::new(),
            tag: String::new(),
        };
        assert_eq!(args.cmd.as_deref(), Some("install"));
        assert!(args.json);
        assert!(is_cli_mode(&args));
    }

    #[test]
    fn parse_args_empty_is_not_cli() {
        let args = CliArgs {
            cmd: None,
            json: false,
            registry: String::new(),
            token: String::new(),
            tag: String::new(),
        };
        assert!(!is_cli_mode(&args));
    }

    #[test]
    fn json_escape_works() {
        assert_eq!(serde_json::to_string("a\"b").unwrap(), "\"a\\\"b\"");
        assert_eq!(serde_json::to_string("简单中文").unwrap(), "\"简单中文\"");
    }
}
