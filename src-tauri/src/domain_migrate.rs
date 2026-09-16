//! 旧域名 → 新域名迁移（`*.ict.cmcc` → `*.ai.ict.cmcc`）。
//!
//! ## 为什么需要
//!
//! 公司把服务从旧环境（本机 Caddy，`ai-*` 前缀）迁到了新 K8S 环境
//! （Higress，`<服务>.ai.ict.cmcc`）。**旧域名已全部下线**（2026-09-14 实测：
//! `ai-conf.ict.cmcc` / `ai-market.ict.cmcc` / `ai-roster.ict.cmcc` 等
//! 全部 HTTP 000 连接失败；对应的新域名全部 200）。
//!
//! 问题在于：**存量同事的 `launcher-config.json` 里存的是旧域名**
//! （他们首次安装时写入的 `serverUrl: http://ai-conf.ict.cmcc`）。
//! 配置一旦落盘就是「用户显式设置」，内置默认值改了也**不会**覆盖它
//! → 同事升级 launcher 后仍然去连已死的旧域名
//! → 日志出现 `FETCH_CONFIG_HTTP_502` / `FETCH_CONFIG_FAILED`，
//!   同步拿不到插件清单、拿不到环境配置，表现为「装了但用不了」。
//!
//! 所以必须在加载配置时做**一次性迁移**：把已知的旧域名替换为新域名，
//! 并立即落盘（否则下次启动又要迁移一遍）。
//!
//! ## 命名规律（不能靠换后缀互转）
//!
//! 旧环境前缀带 `ai-`，新环境是 `<服务>.ai.ict.cmcc`：
//!
//! | 服务 | 旧域名 | 新域名 |
//! |---|---|---|
//! | 配置中心 | `ai-conf.ict.cmcc` | `conf.ai.ict.cmcc` |
//! | 门户 | `ai-market.ict.cmcc` | `market.ai.ict.cmcc` |
//! | 门户管理 | `ai-market-admin.ict.cmcc` | `market-admin.ai.ict.cmcc` |
//! | 岗位发布台 | `ai-job.ict.cmcc` | `job.ai.ict.cmcc` |
//! | 花名册 | `ai-roster.ict.cmcc` | `roster.ai.ict.cmcc` |
//! | 测试台 | `ai-test.ict.cmcc` | `test.ai.ict.cmcc` |
//! | 网关控制台 | `ai-gateway.ict.cmcc` | `gateway.ai.ict.cmcc` |
//! | 认证 | `ai-auth.ict.cmcc` | `auth.ict.cmcc` |
//!
//! **注意 `im-ipm.ict.cmcc`（Matrix homeserver）不在迁移范围**——
//! 它是承载网地址（172.21.163.150），新旧环境共用，没有 `.ai.` 版本。
//! 同理 `registry.ict.cmcc`（npm 私服）实测仍可用（200），保持不动。

/// 旧域名 → 新域名映射表。
///
/// 顺序无关；替换时按「最长匹配优先」避免 `ai-market.ict.cmcc` 被
/// `ai-market-admin` 之类的短前缀误伤（见 `migrate_url`）。
pub const DOMAIN_MIGRATIONS: &[(&str, &str)] = &[
    ("ai-conf.ict.cmcc", "conf.ai.ict.cmcc"),
    ("ai-market-admin.ict.cmcc", "market-admin.ai.ict.cmcc"),
    ("ai-market.ict.cmcc", "market.ai.ict.cmcc"),
    ("ai-job.ict.cmcc", "job.ai.ict.cmcc"),
    ("ai-roster.ict.cmcc", "roster.ai.ict.cmcc"),
    ("ai-test.ict.cmcc", "test.ai.ict.cmcc"),
    ("ai-gateway-admin.ict.cmcc", "gateway-admin.ai.ict.cmcc"),
    ("ai-gateway.ict.cmcc", "gateway.ai.ict.cmcc"),
    ("ai-auth.ict.cmcc", "auth.ict.cmcc"),
];

/// 把单个 URL 里的旧域名换成新域名；无需迁移时原样返回。
///
/// 只替换**主机名部分**（要求域名前是 `//` 或字符串开头，
/// 后面是 `/`、`:`、`?`、`#` 或结尾），避免把路径里恰好出现的同名片段也换掉。
pub fn migrate_url(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return url.to_string();
    }
    // 长域名优先（ai-market-admin 要先于 ai-market 匹配）
    let mut pairs: Vec<&(&str, &str)> = DOMAIN_MIGRATIONS.iter().collect();
    pairs.sort_by_key(|(old, _)| std::cmp::Reverse(old.len()));

    for (old, new) in pairs {
        if let Some(pos) = find_host_occurrence(trimmed, old) {
            let mut out = String::with_capacity(trimmed.len() + 8);
            out.push_str(&trimmed[..pos]);
            out.push_str(new);
            out.push_str(&trimmed[pos + old.len()..]);
            return out;
        }
    }
    url.to_string()
}

/// 在字符串里找 `host` 作为**主机名**出现的位置（不是路径片段）。
///
/// 判定：出现位置的前面必须是 `//`（协议分隔）或串首；
/// 后面必须是 `/`、`:`、`?`、`#` 或串尾。
fn find_host_occurrence(s: &str, host: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut from = 0usize;
    while let Some(rel) = s[from..].find(host) {
        let pos = from + rel;
        let before_ok = pos == 0
            || (pos >= 2 && &s[pos - 2..pos] == "//")
            || (pos >= 1 && &s[pos - 1..pos] == "@"); // 形如 user@host
        let after = pos + host.len();
        let after_ok = after >= bytes.len()
            || matches!(bytes[after], b'/' | b':' | b'?' | b'#' | b'"' | b'\'');
        if before_ok && after_ok {
            return Some(pos);
        }
        from = pos + host.len();
    }
    None
}

/// 在**原始 JSON 层**迁移旧域名，返回是否改动。
///
/// 为什么要在 JSON 层做（而不是先反序列化成 `LauncherConfig` 再写回）：
/// `load_config` 的结果是「内置默认 ⊕ 用户文件」的合并体，
/// 直接 `save_config` 会把**内置默认值全部固化进用户文件**，
/// 导致以后升级 launcher 时新的内置默认再也覆盖不进来。
/// 在 JSON 层只改用户真正写过的字段，保持用户文件语义不变。
pub fn migrate_json(value: &mut serde_json::Value) -> bool {
    let mut changed = false;

    // serverUrl
    if let Some(s) = value.get("serverUrl").and_then(|v| v.as_str()) {
        let m = migrate_url(s);
        if m != s {
            log::info!("域名迁移：serverUrl {s} → {m}（旧环境已下线）");
            value["serverUrl"] = serde_json::Value::String(m);
            changed = true;
        }
    }

    // quickLinks[].url
    if let Some(links) = value.get_mut("quickLinks").and_then(|v| v.as_array_mut()) {
        for l in links.iter_mut() {
            if let Some(u) = l.get("url").and_then(|v| v.as_str()) {
                let m = migrate_url(u);
                if m != u {
                    let label = l.get("label").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    log::info!("域名迁移：菜单「{label}」{u} → {m}");
                    l["url"] = serde_json::Value::String(m);
                    changed = true;
                }
            }
        }
    }

    // mirrorSettings.registry / dshMirrorUrl
    if let Some(ms) = value.get_mut("mirrorSettings").and_then(|v| v.as_object_mut()) {
        for key in ["registry", "dshMirrorUrl"] {
            if let Some(v) = ms.get(key).and_then(|v| v.as_str()) {
                let m = migrate_url(v);
                if m != v {
                    log::info!("域名迁移：mirrorSettings.{key} {v} → {m}");
                    ms.insert(key.to_string(), serde_json::Value::String(m));
                    changed = true;
                }
            }
        }
    }

    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造只有 serverUrl 的用户配置文件 JSON。
    fn json_with(server: &str) -> serde_json::Value {
        serde_json::json!({ "serverUrl": server })
    }

    #[test]
    fn migrates_server_url() {
        let mut v = json_with("http://ai-conf.ict.cmcc");
        assert!(migrate_json(&mut v));
        assert_eq!(v["serverUrl"], "http://conf.ai.ict.cmcc");
    }

    #[test]
    fn leaves_new_domain_untouched() {
        let mut v = json_with("http://conf.ai.ict.cmcc");
        assert!(!migrate_json(&mut v), "新域名不应被改动");
        assert_eq!(v["serverUrl"], "http://conf.ai.ict.cmcc");
    }

    #[test]
    fn does_not_touch_matrix_or_registry() {
        // Matrix homeserver 与 npm 私服不在迁移范围（新旧环境共用，实测仍可用）
        let mut v = json_with("https://im-ipm.ict.cmcc");
        assert!(!migrate_json(&mut v), "im-ipm 不应迁移");
        assert_eq!(v["serverUrl"], "https://im-ipm.ict.cmcc");

        let mut v2 = serde_json::json!({
            "serverUrl": "http://conf.ai.ict.cmcc",
            "mirrorSettings": { "registry": "http://registry.ict.cmcc" }
        });
        assert!(!migrate_json(&mut v2), "registry 不应迁移");
        assert_eq!(v2["mirrorSettings"]["registry"], "http://registry.ict.cmcc");
    }

    #[test]
    fn missing_fields_are_safe() {
        // 空对象 / 无相关字段 → 不改动、不 panic
        let mut v = serde_json::json!({});
        assert!(!migrate_json(&mut v));
        let mut v2 = serde_json::json!({ "port": 3180, "profile": "matrix" });
        assert!(!migrate_json(&mut v2));
        assert_eq!(v2["port"], 3180);
    }

    #[test]
    fn longest_match_wins() {
        // ai-market-admin 必须优先于 ai-market 匹配，否则会得到 market-admin 错误结果
        assert_eq!(
            migrate_url("http://ai-market-admin.ict.cmcc"),
            "http://market-admin.ai.ict.cmcc"
        );
        assert_eq!(
            migrate_url("http://ai-market.ict.cmcc"),
            "http://market.ai.ict.cmcc"
        );
        assert_eq!(
            migrate_url("http://ai-gateway-admin.ict.cmcc"),
            "http://gateway-admin.ai.ict.cmcc"
        );
    }

    #[test]
    fn preserves_port_path_and_scheme() {
        assert_eq!(
            migrate_url("http://ai-conf.ict.cmcc:8080/api/config"),
            "http://conf.ai.ict.cmcc:8080/api/config"
        );
        assert_eq!(
            migrate_url("https://ai-roster.ict.cmcc/"),
            "https://roster.ai.ict.cmcc/"
        );
        assert_eq!(
            migrate_url("http://ai-test.ict.cmcc/admin?x=1#frag"),
            "http://test.ai.ict.cmcc/admin?x=1#frag"
        );
    }

    #[test]
    fn does_not_replace_path_segment_that_looks_like_host() {
        // 路径里出现同样文字不应被替换（前面不是 //）
        assert_eq!(
            migrate_url("http://example.com/ai-conf.ict.cmcc/x"),
            "http://example.com/ai-conf.ict.cmcc/x"
        );
    }

    #[test]
    fn migrates_quick_links_and_mirror() {
        let mut v = serde_json::json!({
            "serverUrl": "http://ai-conf.ict.cmcc",
            "quickLinks": [
                { "label": "门户", "url": "http://ai-market.ict.cmcc" },
                { "label": "花名册", "url": "http://ai-roster.ict.cmcc" }
            ],
            "mirrorSettings": {
                "registry": "http://registry.ict.cmcc",
                "dshMirrorUrl": "http://ai-conf.ict.cmcc/dsh/"
            }
        });
        assert!(migrate_json(&mut v));
        assert_eq!(v["quickLinks"][0]["url"], "http://market.ai.ict.cmcc");
        assert_eq!(v["quickLinks"][1]["url"], "http://roster.ai.ict.cmcc");
        // label 不能被破坏
        assert_eq!(v["quickLinks"][0]["label"], "门户");
        // registry 不动，dshMirrorUrl 迁移
        assert_eq!(v["mirrorSettings"]["registry"], "http://registry.ict.cmcc");
        assert_eq!(v["mirrorSettings"]["dshMirrorUrl"], "http://conf.ai.ict.cmcc/dsh/");
    }

    #[test]
    fn empty_and_plain_values_safe() {
        assert_eq!(migrate_url(""), "");
        assert_eq!(migrate_url("   "), "   ");
        assert_eq!(migrate_url("http://127.0.0.1:8081"), "http://127.0.0.1:8081");
        assert_eq!(migrate_url("http://localhost:3180"), "http://localhost:3180");
    }
}
