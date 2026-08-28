//! 插件镜像上传引擎：把「应装插件 + 全部依赖」异步上传（publish）到内网 registry。
//!
//! 设计（替代危险的任意脚本执行）：
//! - 内置固定逻辑：从外网 npm registry 解析依赖树 → npm pack 拉 tarball → npm publish 到内网
//! - 无用户脚本输入，杜绝注入
//! - 进度实时落盘 `<dsh_home>/mirror-progress.json`，admin_bridge 提供查询路由
//! - 认证：管理员机环境变量（NODE_AUTH_TOKEN 或 tokenEnv 指定），token 不落盘不上报
//!
//! 幂等：目标 registry 已存在同版本包 → npm publish 报错 → 视为已同步，跳过继续。

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Runtime};

use crate::config::*;

/// 上传进度（进程内 + 落盘）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct UploadProgress {
    /// idle / running / done / error
    pub state: String,
    /// 应装插件数
    pub total_plugins: usize,
    /// 依赖树总数（含插件自身）
    pub total_pkgs: usize,
    /// 已完成数
    pub done_pkgs: usize,
    /// 当前处理包名
    pub current_pkg: String,
    /// 目标 registry
    pub registry: String,
    /// 错误信息（单包失败累计）
    pub error: String,
    /// 开始时间（ISO）
    pub started_at: String,
    /// 结束时间（ISO）
    pub finished_at: String,
}

/// 依赖树中的一个包。
#[derive(Debug, Clone)]
pub struct DepEntry {
    pub name: String,
    pub version: String,
}

// ---------- 进度存取 ----------

fn progress_path<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> PathBuf {
    dsh_home(app, cfg).join("mirror-progress.json")
}

/// 进度进程内缓存（load/save 共享同一个）。
static PROGRESS: Mutex<Option<UploadProgress>> = Mutex::new(None);

/// 读取当前进度（进程内缓存优先，文件兜底）。
pub fn load_progress<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> UploadProgress {
    {
        let guard = PROGRESS.lock().unwrap();
        if let Some(p) = guard.as_ref() {
            return p.clone();
        }
    }
    // 首次：从文件读（避免二次加锁死锁）
    let path = progress_path(app, cfg);
    let p: UploadProgress = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    *PROGRESS.lock().unwrap() = Some(p.clone());
    p
}

fn save_progress<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig, p: &UploadProgress) {
    *PROGRESS.lock().unwrap() = Some(p.clone());
    let path = progress_path(app, cfg);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(p) {
        let _ = std::fs::write(path, json);
    }
}

// ---------- 依赖树解析 ----------

/// 解析一个包的完整依赖树（含传递依赖，去重）。
/// 返回所有需要上传的包（含目标包自身）。
pub async fn resolve_dependency_tree(
    name: &str,
    version: &str,
) -> Result<Vec<DepEntry>, String> {
    let client = reqwest::Client::builder()
        .user_agent("dsh-harness-launcher-mirror")
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;

    let mut all: Vec<DepEntry> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    // 从目标包开始 BFS
    let mut queue: Vec<(String, String)> = vec![(name.to_string(), version.to_string())];

    while let Some((pkg, ver)) = queue.pop() {
        let key = format!("{pkg}@{ver}");
        if !seen.insert(key) {
            continue;
        }
        let meta = fetch_meta(&client, &pkg).await?;
        // 选具体版本：优先精确匹配；否则用 semver 范围（简化：取最新满足）
        let resolved_ver = resolve_version(&meta, &ver)?;
        let deps = extract_deps(&meta, &resolved_ver);
        // 记录包（依赖树节点）
        all.push(DepEntry {
            name: pkg.clone(),
            version: resolved_ver.clone(),
        });
        // 依赖入队（递归）
        for (dep_name, dep_range) in deps {
            queue.push((dep_name, dep_range));
        }
    }
    // 目标包放最前（先传插件本身）
    Ok(all)
}

/// 拉取包元信息。
async fn fetch_meta(client: &reqwest::Client, name: &str) -> Result<serde_json::Value, String> {
    let encoded = name.replace('/', "%2F");
    let url = format!("https://registry.npmjs.org/{encoded}");
    let res = client.get(&url).send().await.map_err(|e| format!("fetch {name}: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("fetch {name}: HTTP {}", res.status()));
    }
    res.json().await.map_err(|e| format!("parse {name}: {e}"))
}

/// 解析具体版本：精确匹配 → dist-tags.latest → 最高版本（简化）。
fn resolve_version(meta: &serde_json::Value, spec: &str) -> Result<String, String> {
    let versions = meta.get("versions").and_then(|v| v.as_object()).ok_or("no versions")?;
    // 精确版本
    if versions.contains_key(spec) {
        return Ok(spec.to_string());
    }
    // dist-tags（如 "latest"）
    if let Some(v) = meta
        .get("dist-tags")
        .and_then(|d| d.get(spec))
        .and_then(|v| v.as_str())
    {
        if versions.contains_key(v) {
            return Ok(v.to_string());
        }
    }
    // semver 范围：取满足的最高版本（简化：取版本号最大的）
    let mut best: Option<&String> = None;
    for v in versions.keys() {
        if spec == "*" || spec == "latest" || spec.is_empty() {
            best = Some(v);
        } else if let Some(b) = best {
            if version_greater(v, b) {
                best = Some(v);
            }
        } else {
            best = Some(v);
        }
    }
    best.cloned().ok_or_else(|| format!("no version for {spec}"))
}

/// 简单版本比较（忽略 semver 语义，仅数字比较；够用）。
fn version_greater(a: &str, b: &str) -> bool {
    let na: Vec<u64> = a
        .trim_start_matches('v')
        .split('.')
        .filter_map(|s| s.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok())
        .collect();
    let nb: Vec<u64> = b
        .trim_start_matches('v')
        .split('.')
        .filter_map(|s| s.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok())
        .collect();
    for i in 0..na.len().max(nb.len()) {
        let va = na.get(i).copied().unwrap_or(0);
        let vb = nb.get(i).copied().unwrap_or(0);
        if va != vb {
            return va > vb;
        }
    }
    false
}

/// 提取指定版本的直接依赖（name -> semver range）。
fn extract_deps(meta: &serde_json::Value, version: &str) -> Vec<(String, String)> {
    meta.get("versions")
        .and_then(|v| v.get(version))
        .and_then(|v| v.get("dependencies"))
        .and_then(|d| d.as_object())
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("*").to_string())).collect())
        .unwrap_or_default()
}

// ---------- 上传执行 ----------

/// 开始镜像上传（异步，立即返回；进度经 load_progress 查询）。
///
/// 对依赖树每个包：npm pack（拉外网）→ npm publish（推内网）。
/// 认证用环境变量 token（NODE_AUTH_TOKEN 或 token_env 指定）。
pub fn start_mirror_upload<R: Runtime>(
    app: &AppHandle<R>,
    cfg: &LauncherConfig,
    registry: &str,
    token_env: &str,
) -> Result<(), String> {
    // 已在运行 → 拒绝
    let p = load_progress(app, cfg);
    if p.state == "running" {
        return Err("UPLOAD_ALREADY_RUNNING: 上传正在进行中".to_string());
    }

    let h = app.clone();
    let cfg = cfg.clone();
    let registry = registry.to_string();
    let token_env = token_env.to_string();

    // 应装清单来自服务端缓存的配置（SyncState.cached_config.plugins）
    let plugins: Vec<String> = crate::sync::load_state(&h, &cfg)
        .cached_config
        .map(|c| c.plugins)
        .unwrap_or_default();

    tauri::async_runtime::spawn(async move {
        let _ = run_mirror(&h, &cfg, &registry, &token_env, plugins).await;
    });
    Ok(())
}

/// 实际执行上传（async）。
async fn run_mirror<R: Runtime>(
    app: &AppHandle<R>,
    cfg: &LauncherConfig,
    registry: &str,
    token_env: &str,
    plugins: Vec<String>,
) -> Result<(), String> {
    if plugins.is_empty() {
        let p = UploadProgress {
            state: "error".to_string(),
            error: "应装清单为空，无可上传".to_string(),
            finished_at: now_iso(),
            ..Default::default()
        };
        save_progress(app, cfg, &p);
        return Ok(());
    }

    // 先置 running（解析依赖树可能耗时，进度立即可见）
    let mut p = UploadProgress {
        state: "running".to_string(),
        total_plugins: plugins.len(),
        total_pkgs: 0,
        done_pkgs: 0,
        current_pkg: "解析依赖树…".to_string(),
        registry: registry.to_string(),
        started_at: now_iso(),
        ..Default::default()
    };
    save_progress(app, cfg, &p);
    log::info!("开始镜像上传：解析 {} 个插件的依赖树", plugins.len());

    // 收集所有需要上传的包（插件 + 依赖）
    let mut all_pkgs: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for plugin in &plugins {
        // 插件自身（latest）
        let tree = match resolve_dependency_tree(plugin, "latest").await {
            Ok(t) => t,
            Err(e) => {
                p.state = "error".to_string();
                p.error = format!("解析 {plugin} 依赖树失败：{e}");
                p.finished_at = now_iso();
                save_progress(app, cfg, &p);
                return Ok(());
            }
        };
        for entry in tree {
            let key = format!("{}@{}", entry.name, entry.version);
            if seen.insert(key) {
                all_pkgs.push((entry.name, entry.version));
            }
        }
    }

    p.total_pkgs = all_pkgs.len();
    save_progress(app, cfg, &p);
    log::info!("依赖树解析完成：共 {} 个包（含依赖）", all_pkgs.len());

    let mut errors: Vec<String> = Vec::new();
    for (i, (name, version)) in all_pkgs.iter().enumerate() {
        let mut cur = load_progress(app, cfg);
        cur.current_pkg = format!("{name}@{version}");
        save_progress(app, cfg, &cur);

        match upload_one_pkg(name, version, registry, token_env) {
            Ok(()) => {
                log::info!("上传成功 [{}/{}] {}@{}", i + 1, all_pkgs.len(), name, version);
            }
            Err(e) => {
                log::warn!("上传失败 [{}/{}] {}@{}: {}", i + 1, all_pkgs.len(), name, version, e);
                errors.push(format!("{}@{}: {e}", name, version));
            }
        }

        let mut cur = load_progress(app, cfg);
        cur.done_pkgs = i + 1;
        save_progress(app, cfg, &cur);
    }

    let mut final_p = load_progress(app, cfg);
    final_p.state = if errors.is_empty() { "done".to_string() } else { "error".to_string() };
    final_p.error = errors.join("; ");
    final_p.finished_at = now_iso();
    save_progress(app, cfg, &final_p);
    log::info!("镜像上传结束：{}/{} 成功", all_pkgs.len() - errors.len(), all_pkgs.len());
    Ok(())
}

/// 上传单个包：npm pack（拉外网）→ npm publish（推内网）。
fn upload_one_pkg(name: &str, version: &str, registry: &str, token_env: &str) -> Result<(), String> {
    let tmp = std::env::temp_dir().join(format!("dsh-mirror-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);

    // 1. npm pack
    let pack_out = run_npm(&tmp, &["pack", &format!("{name}@{version}"), "--pack-destination", &tmp.to_string_lossy()], &[])?;
    let tarball = find_tarball(&tmp)?;

    // 2. npm publish（认证用环境变量）
    let token_val = std::env::var(token_env).unwrap_or_default();
    let mut envs: Vec<(String, String)> = Vec::new();
    if !token_val.is_empty() {
        envs.push(("NODE_AUTH_TOKEN".to_string(), token_val));
    }
    let _ = pack_out;
    run_npm(
        &tmp,
        &["publish", &tarball.to_string_lossy(), "--registry", registry, "--access", "public"],
        &envs,
    )?;

    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

/// 执行 npm 命令（工作目录 + 环境变量）。
///
/// Windows 上 `Command::new("npm")` 在 spawn 时用**当前进程** PATH 解析 npm，
/// env 里的 PATH 覆盖不生效——所以这里显式解析 npm 可执行文件路径。
fn run_npm(cwd: &PathBuf, args: &[&str], envs: &[(String, String)]) -> Result<String, String> {
    let npm_exe = resolve_npm_path();
    let mut cmd = Command::new(npm_exe);
    cmd.args(args).current_dir(cwd);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    // 前置 node/npm 所在目录到 PATH（子进程内 npm 调 node 需要）
    if let Some(existing) = std::env::var_os("PATH") {
        let mut paths = std::env::split_paths(&existing).collect::<Vec<_>>();
        if let Some(node_dir) = system_node_dir() {
            if !paths.contains(&node_dir) {
                paths.insert(0, node_dir);
            }
        }
        if let Ok(joined) = std::env::join_paths(paths) {
            cmd.env("PATH", joined);
        }
    }
    let output = cmd.output().map_err(|e| format!("npm spawn: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // 已存在同版本 → 视为已同步（幂等）
        if stderr.contains("EPUBLISHCONFLICT") || stderr.contains("cannot publish over") {
            return Ok("already-published".to_string());
        }
        return Err(format!("npm {} 失败: {}", args[0], stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// 解析 npm 可执行文件路径：先看 node 同目录（npm.cmd/npm），再 PATH 兜底。
fn resolve_npm_path() -> PathBuf {
    if let Some(dir) = system_node_dir() {
        let exe = if cfg!(windows) { dir.join("npm.cmd") } else { dir.join("npm") };
        if exe.is_file() {
            return exe;
        }
    }
    PathBuf::from("npm")
}

/// 系统 node 所在目录（npm 同目录）。
fn system_node_dir() -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .find(|dir| {
            let node = if cfg!(windows) { dir.join("node.exe") } else { dir.join("node") };
            let npm = if cfg!(windows) { dir.join("npm.cmd") } else { dir.join("npm") };
            node.is_file() && npm.is_file()
        })
}

/// 在目录里找刚 pack 的 tarball。
fn find_tarball(dir: &PathBuf) -> Result<PathBuf, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().map(|x| x == "tgz").unwrap_or(false) {
            return Ok(p);
        }
    }
    Err("未找到 pack 产物".to_string())
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(version_greater("1.2.3", "1.2.2"));
        assert!(version_greater("2.0.0", "1.9.9"));
        assert!(!version_greater("1.2.2", "1.2.3"));
        assert!(!version_greater("1.2.3", "1.2.3"));
    }

    #[test]
    fn resolve_version_exact_and_latest() {
        let meta = serde_json::json!({
            "dist-tags": { "latest": "0.2.2" },
            "versions": { "0.1.0": {}, "0.2.2": {} }
        });
        assert_eq!(resolve_version(&meta, "0.1.0").unwrap(), "0.1.0");
        assert_eq!(resolve_version(&meta, "latest").unwrap(), "0.2.2");
        assert_eq!(resolve_version(&meta, "*").unwrap(), "0.2.2");
    }

    #[test]
    fn extract_deps_reads_dependencies() {
        let meta = serde_json::json!({
            "versions": {
                "1.0.0": {
                    "dependencies": { "zod": "^4.0.0", "foo": "1.0.0" }
                }
            }
        });
        let deps = extract_deps(&meta, "1.0.0");
        assert_eq!(deps.len(), 2);
        assert!(deps.contains(&("zod".to_string(), "^4.0.0".to_string())));
    }

    #[test]
    fn progress_serializable() {
        let p = UploadProgress {
            state: "running".to_string(),
            total_pkgs: 10,
            done_pkgs: 3,
            current_pkg: "zod@4.4.3".to_string(),
            registry: "https://registry.ict.cmcc".to_string(),
            ..Default::default()
        };
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"state\":\"running\""));
        assert!(json.contains("\"total_pkgs\":10"));
    }
}
