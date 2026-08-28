//! 加速源测速：对 npm registry 与 GitHub 中转源发探测请求，测延迟/可达性。
//!
//! 托盘「加速设置 → 测速」调用：本机直接测当前配置 + 常用源，
//! 结果按延迟排序，帮助用户/管理员选出最快的源。
//!
//! 测速目标 URL（小且稳定）：
//! - npm registry：`<registry>/dsh-nested-followups`（包元信息 JSON，几 KB）
//! - GitHub 直连：`https://github.com/`（首页）
//! - GitHub 镜像：`<prefix>https://github.com/`（镜像前缀 + 目标 URL）

use std::time::Duration;
use tauri::{AppHandle, Runtime};

use crate::config::*;

/// 单个源的测速结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SpeedResult {
    /// 源名（展示用）
    pub name: String,
    /// 测速 URL
    pub url: String,
    /// HTTP 状态（0 = 连接失败/超时）
    pub status: u16,
    /// 延迟毫秒（失败 = 超时时间）
    pub latency_ms: u64,
    /// 是否可用
    pub ok: bool,
}

/// 测速超时（单个源）。
const SPEED_TIMEOUT_MS: u64 = 6000;

/// npm 常用源（测速候选）。
const NPM_CANDIDATES: &[(&str, &str)] = &[
    ("npmjs 官方", "https://registry.npmjs.org"),
    ("npmmirror 镜像", "https://registry.npmmirror.com"),
];

/// GitHub 常用中转（测速候选）。
const GH_CANDIDATES: &[(&str, &str)] = &[
    ("GitHub 直连", ""),
    ("ghfast.top", "https://ghfast.top/"),
    ("ghproxy.net", "https://ghproxy.net/"),
];

/// 对 npm registry 源测速：当前配置（显式）∪ 常用候选，去重。
pub async fn speedtest_npm<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> Vec<SpeedResult> {
    let configured = resolve_npm_registries(cfg);
    let mut urls: Vec<(String, String)> = configured
        .iter()
        .map(|r| (r.clone(), r.clone()))
        .collect();
    for (name, base) in NPM_CANDIDATES {
        let url = format!("{base}/dsh-nested-followups");
        if !urls.iter().any(|(_, u)| *u == url) {
            urls.push((name.to_string(), url));
        }
    }
    let _ = app;
    run_speedtests(urls).await
}

/// 对 GitHub 中转源测速：当前配置（显式）∪ 常用候选，去重。
pub async fn speedtest_gh<R: Runtime>(app: &AppHandle<R>, cfg: &LauncherConfig) -> Vec<SpeedResult> {
    let configured = resolve_gh_prefixes(cfg);
    let target = "https://github.com/";
    let mut urls: Vec<(String, String)> = configured
        .iter()
        .map(|p| (p.clone(), format!("{p}{target}")))
        .collect();
    for (name, prefix) in GH_CANDIDATES {
        let url = if prefix.is_empty() {
            target.to_string()
        } else {
            format!("{prefix}{target}")
        };
        if !urls.iter().any(|(_, u)| *u == url) {
            urls.push((name.to_string(), url));
        }
    }
    let _ = app;
    run_speedtests(urls).await
}

/// 并发测速一组 URL，按延迟升序返回。
async fn run_speedtests(urls: Vec<(String, String)>) -> Vec<SpeedResult> {
    let client = reqwest::Client::builder()
        .user_agent("deepseek-harness-launcher-speedtest")
        .connect_timeout(Duration::from_millis(SPEED_TIMEOUT_MS))
        .timeout(Duration::from_millis(SPEED_TIMEOUT_MS))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let mut tasks = Vec::new();
    for (name, url) in urls {
        let client = client.clone();
        tasks.push(tokio::spawn(async move {
            let started = std::time::Instant::now();
            match client.get(&url).send().await {
                Ok(res) => {
                    let latency = started.elapsed().as_millis() as u64;
                    SpeedResult {
                        name,
                        url,
                        status: res.status().as_u16(),
                        latency_ms: latency,
                        ok: res.status().is_success(),
                    }
                }
                Err(_) => SpeedResult {
                    name,
                    url,
                    status: 0,
                    latency_ms: started.elapsed().as_millis() as u64,
                    ok: false,
                },
            }
        }));
    }

    let mut results: Vec<SpeedResult> = Vec::new();
    for t in tasks {
        if let Ok(r) = t.await {
            results.push(r);
        }
    }
    // 可达的在前（按延迟），失败的在后
    results.sort_by(|a, b| {
        b.ok
            .cmp(&a.ok)
            .then(a.latency_ms.cmp(&b.latency_ms))
    });
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_result_shape() {
        let r = SpeedResult {
            name: "test".to_string(),
            url: "https://x".to_string(),
            status: 200,
            latency_ms: 10,
            ok: true,
        };
        assert!(r.ok);
        assert_eq!(r.status, 200);
        // 可序列化（管理页/通知用）
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"ok\":true"));
    }

    #[test]
    fn sort_ok_first_then_by_latency() {
        let mut v = vec![
            SpeedResult { name: "slow".into(), url: "u".into(), status: 200, latency_ms: 900, ok: true },
            SpeedResult { name: "fail".into(), url: "u".into(), status: 0, latency_ms: 6000, ok: false },
            SpeedResult { name: "fast".into(), url: "u".into(), status: 200, latency_ms: 100, ok: true },
        ];
        v.sort_by(|a, b| b.ok.cmp(&a.ok).then(a.latency_ms.cmp(&b.latency_ms)));
        assert_eq!(v[0].name, "fast");
        assert_eq!(v[1].name, "slow");
        assert_eq!(v[2].name, "fail");
    }
}
