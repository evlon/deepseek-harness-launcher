//! 安装引擎：各依赖的下载 / 校验 / 解压。
//!
//! 独立重实现 `deepseek-harness-desktop` 的 `service/download`，参照其常量与校验逻辑。
//! 支持多下载源（官方直连 → ghfast.top 镜像兜底）、断点续传重试、SHA-256 完整性校验、
//! 以及解压后的单顶层目录摊平 + 原子切换。

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Runtime};

use crate::config::*;

/// 一个可安装组件（Node / pnpm / dsh / Git）。
pub enum Component {
    Node,
    Pnpm,
    Dsh,
    #[cfg(windows)]
    Git,
}

impl Component {
    /// 展示名（日志用）
    pub fn title(&self) -> &'static str {
        match self {
            Component::Node => "运行环境 Node.js",
            Component::Pnpm => "pnpm 包管理器",
            Component::Dsh => "Harness 核心",
            #[cfg(windows)]
            Component::Git => "Git 环境",
        }
    }

    /// 是否已在盘（Node 额外校验版本与期望一致，避免半成品安装被跳过）。
    pub fn check_installed<R: Runtime>(&self, app: &AppHandle<R>) -> bool {
        match self {
            Component::Node => node_installed_ok(app),
            Component::Pnpm => pnpm_binary_path(app).exists(),
            Component::Dsh => dsh_binary_path(app).exists(),
            #[cfg(windows)]
            Component::Git => {
                if git_binary_path(app).exists() {
                    return true;
                }
                system_git_available()
            }
        }
    }

    /// 安装目标目录
    fn install_dest<R: Runtime>(&self, app: &AppHandle<R>) -> PathBuf {
        match self {
            Component::Node => runtime_path(app),
            Component::Pnpm => pnpm_install_path(app),
            Component::Dsh => dsh_install_path(app),
            #[cfg(windows)]
            Component::Git => git_install_path(app),
        }
    }

    /// 下载并安装该组件（含校验）。
    pub async fn install<R: Runtime>(&self, app: &AppHandle<R>, on_progress: ProgressCallback<'_>) -> Result<(), String> {
        log::info!("开始安装组件：{}", self.title());
        match self {
            Component::Node => {
                let (url, filename) = node_download_url();
                let sha = fetch_node_sha256(&filename).await.ok();
                let buf = download_bytes(&[url], on_progress).await?;
                if let Some(expected) = &sha {
                    verify_sha256(&buf, expected)?;
                }
                ensure_extract(&filename, buf, self.install_dest(app)).await?;
            }
            Component::Pnpm => {
                let urls = pnpm_download_urls();
                let buf = download_bytes(&urls, on_progress).await?;
                verify_sha256(&buf, PNPM_SHA256)?;
                ensure_extract("pnpm.tgz", buf, self.install_dest(app)).await?;
            }
            Component::Dsh => {
                let release = fetch_latest_dsh().await?;
                log::info!("最新 Harness 发行版：{}", release.tag);
                let cfg = load_cached();
                // 官方直连 + 全部镜像前缀（多源按序尝试）
                let mut urls = vec![release.asset_url.clone()];
                urls.extend(mirror_urls(&release.asset_url, &cfg));
                let buf = download_bytes(&urls, on_progress).await?;
                if let Some(digest) = &release.digest {
                    verify_sha256(&buf, digest)?;
                } else {
                    log::warn!("未取到可信摘要，跳过 SHA-256 校验（仅作提示，安装仍继续）");
                }
                ensure_extract(&release.asset_name, buf, self.install_dest(app)).await?;
            }
            #[cfg(windows)]
            Component::Git => {
                let (url, filename) = mingit_download_url()?;
                let sha = mingit_sha256()?;
                let buf = download_bytes(&[url], on_progress).await?;
                verify_sha256(&buf, sha)?;
                ensure_extract(&filename, buf, self.install_dest(app)).await?;
            }
        }
        log::info!("组件安装完成：{}", self.title());
        Ok(())
    }
}

// ---------------- 下载原语 ----------------

/// Node 是否已正确安装：二进制存在，且版本与期望的 `NODE_VERSION` 一致。
///
/// 半成品安装（解压中断 / 版本不符）返回 `false`，触发重装，避免后续 `launch`
/// 用残缺的 Node 启动失败。版本探询失败视为未安装（重装更稳妥）。
///
/// 配置 `useSystemNode=true` 时，优先探测 PATH 上的系统 node（版本匹配即视为已安装，
/// 跳过下载自带 node）；默认自包含（只用 launcher 自己的 runtime）。
fn node_installed_ok<R: Runtime>(app: &AppHandle<R>) -> bool {
    let cfg = load_cached();
    if use_system_node(&cfg) {
        if let Some(system) = system_node_path() {
            if node_version_matches(&system) {
                log::info!("使用系统 node（已装且版本匹配）：{}", system.display());
                return true;
            }
            log::info!("系统 node 主版本低于期望 {}，使用自带 node", NODE_VERSION);
        } else {
            log::info!("PATH 上无系统 node，使用自带 node");
        }
    }
    let bin = node_binary_path(app);
    node_version_matches(&bin)
}

/// 系统 PATH 上的 node 路径（Windows: node.exe；Unix: node）。
fn system_node_path() -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| {
            if cfg!(windows) {
                dir.join("node.exe")
            } else {
                dir.join("node")
            }
        })
        .find(|p| p.is_file())
}

/// 执行 node --version 并比对版本（主版本 >= 期望即视为可用，兼容更新的 node）。
fn node_version_matches(bin: &Path) -> bool {
    if !bin.exists() {
        return false;
    }
    let Ok(output) = std::process::Command::new(bin)
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let version = String::from_utf8_lossy(&output.stdout);
    // node --version 输出形如 "v22.22.0\n"；主版本 >= NODE_VERSION 主版本即可
    let min_major = NODE_VERSION
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(22);
    let ver_major = version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    ver_major >= min_major
}

/// 按顺序尝试多个下载源，带 Range 续传重试。
///
/// 加速策略：**第一个源（通常是官方直连）快速失败**——只重试 `FAST_FAIL_ATTEMPTS` 次
/// 就切换到镜像源，避免国内环境在直连 GitHub/npmjs 上浪费大量时间（此前 5 次重试
/// 需 ~100 秒才切镜像）。镜像源是兜底，保留标准重试次数。
///
/// `on_progress` 可选：每收到一块数据回调 `(downloaded, total)`，用于展示下载进度。
pub async fn download_bytes(
    urls: &[String],
    on_progress: ProgressCallback<'_>,
) -> Result<Vec<u8>, String> {
    if urls.is_empty() {
        return Err("DOWNLOAD_URL_EMPTY: 没有提供下载源".to_string());
    }
    let mut last_err = String::new();
    for (index, url) in urls.iter().enumerate() {
        if index > 0 {
            log::warn!("主下载源失败，切换镜像源重试：{url}");
        }
        let attempts = if index == 0 { FAST_FAIL_ATTEMPTS } else { MAX_ATTEMPTS };
        match download_with_retry(url, attempts, on_progress).await {
            Ok(buf) => return Ok(buf),
            Err(e) => last_err = e,
        }
    }
    Err(if urls.len() > 1 {
        format!("{}（已尝试 {} 个下载源）", last_err, urls.len())
    } else {
        last_err
    })
}

/// 下载进度回调（每收到一块数据触发）：`(downloaded_bytes, total_bytes)`。
pub type ProgressCallback<'a> = Option<&'a (dyn Fn(u64, u64) + Send + Sync)>;

/// 镜像源（最后一个兜底）的标准重试次数。
const MAX_ATTEMPTS: usize = 5;
/// 首个源（官方直连）的快速失败次数：国内环境直连 GitHub/npmjs 大概率不通，
/// 快速切换到镜像源提升体验。
const FAST_FAIL_ATTEMPTS: usize = 2;

async fn download_with_retry(
    url: &str,
    attempts: usize,
    on_progress: ProgressCallback<'_>,
) -> Result<Vec<u8>, String> {
    validate_url(url)?;
    let client = reqwest::Client::builder()
        .user_agent("deepseek-harness-launcher")
        .connect_timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;

    let mut buffer: Vec<u8> = Vec::new();
    for attempt in 1..=attempts {
        if attempt > 1 {
            let delay_secs = (1u64 << (attempt - 1)).min(8);
            log::warn!(
                "下载尝试 {}/{} 失败，{}s 后重试（已从 {} 字节续传）",
                attempt - 1,
                attempts,
                delay_secs,
                buffer.len()
            );
            tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
        }
        match download_attempt(&client, url, &mut buffer, on_progress).await {
            Ok(()) => {
                log::info!("下载完成：{} 字节", buffer.len());
                return Ok(buffer);
            }
            Err(e) => log::warn!("下载尝试 {}/{} 失败：{e}", attempt, attempts),
        }
    }
    Err(format!(
        "DOWNLOAD_INTERRUPTED: 下载中断，已自动重试 {attempts} 次仍失败，已下载约 {:.1} MB",
        buffer.len() as f64 / 1_000_000.0
    ))
}

async fn download_attempt(
    client: &reqwest::Client,
    url: &str,
    buffer: &mut Vec<u8>,
    on_progress: ProgressCallback<'_>,
) -> Result<(), String> {
    let resume_from = buffer.len() as u64;
    let mut req = client.get(url);
    if resume_from > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={resume_from}-"));
    }
    let res = req.send().await.map_err(|e| e.to_string())?;
    validate_url(res.url().as_str())?;
    if !res.status().is_success() {
        return Err(format!("下载失败：HTTP {}", res.status()));
    }
    let total_size = if res.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        resume_from + res.content_length().unwrap_or(0)
    } else {
        if resume_from > 0 {
            buffer.clear();
        }
        res.content_length().unwrap_or(0)
    };
    let mut downloaded: u64 = 0;
    let mut stream = res.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        buffer.extend_from_slice(&chunk);
        downloaded += chunk.len() as u64;
        let received = resume_from + downloaded;
        // 进度回调（窗口/托盘实时展示）
        if let Some(cb) = on_progress {
            let total = if total_size > 0 { total_size } else { 0 };
            cb(received, total);
        }
        if total_size > 0 {
            log::debug!("下载进度：{:.1}/{:.1} MB", received as f64 / 1e6, total_size as f64 / 1e6);
        }
    }
    Ok(())
}

fn validate_url(url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("DOWNLOAD_URL_INVALID: {e}"))?;
    let trusted = matches!(
        parsed.host_str(),
        Some(
            "nodejs.org"
                | "registry.npmjs.org"
                | "github.com"
                | "release-assets.githubusercontent.com"
                | "objects.githubusercontent.com"
                | "npmmirror.com"
                | "cdn.npmmirror.com"
                | "registry.npmmirror.com"
                | "ghfast.top"
        )
    );
    if parsed.scheme() != "https" || !trusted {
        return Err(format!("DOWNLOAD_SOURCE_UNTRUSTED: {url}"));
    }
    Ok(())
}

/// 校验 SHA-256。
pub fn verify_sha256(buffer: &[u8], expected: &str) -> Result<(), String> {
    let expected = expected
        .strip_prefix("sha256:")
        .unwrap_or(expected)
        .trim()
        .to_ascii_lowercase();
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("INTEGRITY_METADATA_INVALID: 期望的 SHA-256 非法".to_string());
    }
    let actual = format!("{:x}", Sha256::digest(buffer));
    if actual != expected {
        return Err(format!(
            "INTEGRITY_CHECK_FAILED: SHA-256 不匹配，期望 {expected}，实际 {actual}"
        ));
    }
    Ok(())
}

// ---------------- 解压 ----------------

/// 解压下载内容到 dest（zip / tar.gz 自动识别），摊平单顶层目录后原子切换。
pub async fn ensure_extract(name: &str, buffer: Vec<u8>, dest: PathBuf) -> Result<(), String> {
    log::info!("开始解压：{} -> {}", name, dest.display());
    let pure = name.split('?').next().unwrap_or(name).to_ascii_lowercase();
    let is_tgz = pure.ends_with(".tar.gz") || pure.ends_with(".tgz");
    let is_zip = pure.ends_with(".zip");

    let parent = dest.parent().unwrap_or(Path::new(".")).to_path_buf();
    let leaf = dest
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("pkg")
        .to_string();
    let staging = parent.join(format!(".{leaf}.installing-{}", std::process::id()));
    let _ = remove_path_if_exists(&staging).await;

    if !is_tgz && !is_zip {
        // 非压缩文件：直接写入
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        fs::write(&staging, &buffer).map_err(|e| e.to_string())?;
        commit(staging, dest).await?;
        return Ok(());
    }

    fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    if is_tgz {
        extract_tgz(&buffer, &staging)?;
    } else {
        extract_zip(&buffer, &staging)?;
    }
    flatten_directory(&staging)?;
    commit(staging, dest.clone()).await?;
    log::info!("解压完成：{}", dest.display());
    Ok(())
}

fn extract_zip(buffer: &[u8], dest: &Path) -> Result<(), String> {
    let reader = std::io::Cursor::new(buffer);
    let mut archive = zip::ZipArchive::new(reader).map_err(|e| format!("ZIP_OPEN_FAILED: {e}"))?;
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("ZIP_READ_FAILED: {e}"))?;
        let out_path = match file.enclosed_name() {
            Some(p) => dest.join(p),
            None => continue,
        };
        if file.is_dir() {
            fs::create_dir_all(&out_path).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut out = fs::File::create(&out_path).map_err(|e| e.to_string())?;
            std::io::copy(&mut file, &mut out).map_err(|e| e.to_string())?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = file.unix_mode() {
                let _ = fs::set_permissions(&out_path, fs::Permissions::from_mode(mode));
            }
        }
    }
    Ok(())
}

fn extract_tgz(buffer: &[u8], dest: &Path) -> Result<(), String> {
    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(buffer));
    let mut archive = tar::Archive::new(decoder);
    archive
        .unpack(dest)
        .map_err(|e| format!("TAR_EXTRACT_FAILED: {e}"))?;
    Ok(())
}

/// 若解压目录仅含一个子目录，将其内容提升一级（去掉发行包外层套娃目录）。
fn flatten_directory(dir: &Path) -> Result<(), String> {
    let entries: Vec<_> = fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .collect();
    if entries.len() == 1 && entries[0].path().is_dir() {
        let inner = entries[0].path();
        let tmp = dir.join(format!(".flat-{}", std::process::id()));
        fs::rename(&inner, &tmp).map_err(|e| e.to_string())?;
        // 把 inner 的内容（已改名 tmp）整体上提到 dir
        for entry in fs::read_dir(&tmp).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            fs::rename(entry.path(), dir.join(entry.file_name())).map_err(|e| e.to_string())?;
        }
        fs::remove_dir(&tmp).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub async fn remove_path_if_exists(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        fs::remove_dir_all(path).map_err(|e| e.to_string())?;
    } else {
        fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 原子切换：先清掉旧 dest，再把 staging 重命名为 dest。
async fn commit(staging: PathBuf, dest: PathBuf) -> Result<(), String> {
    if dest.exists() {
        remove_path_if_exists(&dest).await?;
    }
    fs::rename(&staging, &dest).map_err(|e| format!("INSTALL_COMMIT_FAILED: {e}"))?;
    Ok(())
}

// ---------------- GitHub 发行版 / 校验和抓取 ----------------

struct DshRelease {
    tag: String,
    asset_url: String,
    asset_name: String,
    digest: Option<String>,
}

/// 查询 GitHub 最新 Harness 发行版（tag + 资产地址 + 可信摘要）。
async fn fetch_latest_dsh() -> Result<DshRelease, String> {
    let asset_name = dsh_asset_filename()?;
    let client = gh_client()?;
    let api = client
        .get("https://api.github.com/repos/dsh-tauri-desk/deepseek-harness-pkg/releases/latest")
        .send()
        .await;
    match api {
        Ok(res) if res.status().is_success() => {
            let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
            let tag = json
                .get("tag_name")
                .and_then(|v| v.as_str())
                .ok_or("缺少 tag_name")?
                .to_string();
            let (asset_url, digest) = json
                .get("assets")
                .and_then(|v| v.as_array())
                .and_then(|assets| {
                    assets.iter().find(|a| {
                        a.get("name").and_then(|v| v.as_str()) == Some(asset_name.as_str())
                    })
                })
                .map(|asset| {
                    let url = asset
                        .get("browser_download_url")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&fallback_dsh_url(&asset_name))
                        .to_string();
                    let digest = asset
                        .get("digest")
                        .and_then(|v| v.as_str())
                        .filter(|d| d.starts_with("sha256:"))
                        .map(|d| d.to_string());
                    (url, digest)
                })
                .unwrap_or((fallback_dsh_url(&asset_name), None));
            Ok(DshRelease {
                tag,
                asset_url,
                asset_name,
                digest,
            })
        }
        _ => {
            log::warn!("GitHub API 不可用，回退到固定发行版地址（无摘要校验）");
            Ok(DshRelease {
                tag: "latest".to_string(),
                asset_url: fallback_dsh_url(&asset_name),
                asset_name,
                digest: None,
            })
        }
    }
}

fn fallback_dsh_url(asset_name: &str) -> String {
    format!("{DSH_CORE_URL}{asset_name}")
}

fn gh_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("deepseek-harness-launcher")
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())
}

/// 从 Node.js 官方 SHASUMS256.txt 读取指定平台包摘要。
async fn fetch_node_sha256(filename: &str) -> Result<String, String> {
    let url = format!("{NODE_BASE_URL}{NODE_VERSION}/SHASUMS256.txt");
    let text = reqwest::Client::builder()
        .user_agent("deepseek-harness-launcher")
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?
        .get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    text.lines()
        .filter_map(|line| line.split_once(char::is_whitespace))
        .find_map(|(digest, name)| {
            (name.trim_start_matches([' ', '*']) == filename).then(|| digest.to_string())
        })
        .ok_or_else(|| format!("INTEGRITY_METADATA_MISSING: 无 {filename} 的校验和"))
}

// ---------------- 平台资产命名 ----------------

fn dsh_asset_filename() -> Result<String, String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", _) => Ok("deepseek-harness-pkg-windows.zip".to_string()),
        ("macos", "aarch64") => Ok("deepseek-harness-pkg-macos-arm64.zip".to_string()),
        ("macos", "x86_64") => Ok("deepseek-harness-pkg-macos-x64.zip".to_string()),
        ("linux", _) => Ok("deepseek-harness-pkg-linux.zip".to_string()),
        other => Err(format!("不支持的平台：{:?}", other)),
    }
}

fn node_download_url() -> (String, String) {
    let filename = node_pkg_filename();
    let base = match detect_region() {
        Region::Domestic => NODE_MIRROR_BASE_URL,
        Region::Overseas => NODE_BASE_URL,
    };
    (
        format!("{base}{NODE_VERSION}/{filename}"),
        filename,
    )
}

fn node_pkg_filename() -> String {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => format!("node-{NODE_VERSION}-darwin-arm64.tar.gz"),
        ("macos", "x86_64") => format!("node-{NODE_VERSION}-darwin-x64.tar.gz"),
        ("windows", _) => format!("node-{NODE_VERSION}-win-x64.zip"),
        ("linux", "x86_64") => format!("node-{NODE_VERSION}-linux-x64.tar.gz"),
        ("linux", "aarch64") => format!("node-{NODE_VERSION}-linux-arm64.tar.gz"),
        other => format!("node-{NODE_VERSION}-{}.tar.gz", other.0),
    }
}

fn pnpm_download_urls() -> Vec<String> {
    let filename = format!("pnpm-{PNPM_VERSION}.tgz");
    match detect_region() {
        Region::Domestic => vec![format!("{PNPM_MIRROR_BASE_URL}{filename}")],
        Region::Overseas => vec![format!("{PNPM_BASE_URL}{filename}")],
    }
}

#[cfg(windows)]
fn mingit_download_url() -> Result<(String, String), String> {
    let filename = match std::env::consts::ARCH {
        "x86_64" => format!("MinGit-{MINGIT_VERSION}-64-bit.zip"),
        "aarch64" => format!("MinGit-{MINGIT_VERSION}-arm64.zip"),
        arch => return Err(format!("MINGIT_PLATFORM_UNSUPPORTED: windows {arch}")),
    };
    Ok((format!("{MINGIT_BASE_URL}{filename}"), filename))
}

#[cfg(windows)]
fn mingit_sha256() -> Result<&'static str, String> {
    match std::env::consts::ARCH {
        "x86_64" => Ok(MINGIT_X64_SHA256),
        _ => Err("MINGIT_SHA_UNSUPPORTED: 仅内置 x86_64 摘要".to_string()),
    }
}

/// 系统是否已有可用的 git（Windows 上免安装 Git 可跳过）。
#[cfg(windows)]
fn system_git_available() -> bool {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("git.exe"))
        .any(|c| c.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_verify_matches() {
        let digest = format!("{:x}", Sha256::digest(b"hello"));
        assert!(verify_sha256(b"hello", &digest).is_ok());
        // 支持 sha256: 前缀
        assert!(verify_sha256(b"hello", &format!("sha256:{digest}")).is_ok());
        // 大小写不敏感
        assert!(verify_sha256(b"hello", &digest.to_uppercase()).is_ok());
    }

    #[test]
    fn sha256_verify_rejects_mismatch_and_garbage() {
        assert!(verify_sha256(b"hello", "deadbeef").is_err());
        assert!(verify_sha256(b"hello", "").is_err());
        assert!(verify_sha256(b"hello", "not-a-hex-digest").is_err());
    }

    #[test]
    fn url_validation_whitelists_trusted_hosts() {
        assert!(validate_url("https://nodejs.org/dist/v22/x.zip").is_ok());
        assert!(validate_url("https://registry.npmjs.org/pnpm/-/x.tgz").is_ok());
        assert!(validate_url("https://github.com/a/b/releases/download/x.zip").is_ok());
        assert!(validate_url("https://ghfast.top/https://github.com/a/b.zip").is_ok());
        assert!(validate_url("https://registry.npmmirror.com/pnpm/-/x.tgz").is_ok());
    }

    #[test]
    fn url_validation_rejects_untrusted() {
        assert!(validate_url("http://nodejs.org/dist/x.zip").is_err()); // 非 https
        assert!(validate_url("https://evil.example.com/x.zip").is_err());
        assert!(validate_url("https://github.com.evil.com/x.zip").is_err());
        assert!(validate_url("not-a-url").is_err());
    }

    #[test]
    fn node_pkg_filename_covers_platforms() {
        let f = node_pkg_filename();
        assert!(f.starts_with(&format!("node-{NODE_VERSION}")), "got {f}");
    }
}
