//! 环境默认配置下发（launcher 预置 profile 时写入）。
//!
//! ## 为什么需要
//!
//! 同事装完 launcher 后，插件需要知道「内网各服务的地址」才能用。这些地址对
//! 全公司是**统一**的，不该让每个人手填。
//!
//! ## 通道选择（实测依据）
//!
//! 插件配置有两个平面（见 `docs/插件配置下发设计.md`）：
//!
//! | 通道 | 落点 | 承载 |
//! |---|---|---|
//! | `cordis.patch.yml` | `<profile>/cordis.patch.yml` | 插件**行 config** |
//! | `settings.yaml` | `$DSH_HOME/settings.yaml` | 插件 **settings namespace** |
//!
//! 实测：我们 4 个插件的可配置项几乎全在 **settings namespace**
//! （`himarket` / `roster` / `dsh-matrix` / `llm-codebuddy`），
//! 故环境地址写 `settings.yaml`；插件行为开关写 `cordis.patch.yml`。
//!
//! ## 优先级链（实测自 dsh-himarket/src/domain.ts）
//!
//! ```text
//! 1. 代码内置默认档位（new = 新 K8S 环境）        ← 最低
//! 2. DSH_DEPLOY_ENV=legacy 整体切旧环境
//! 3. DSH_DOMAIN_SUFFIX 只换后缀
//! 4. 各服务整条 URL 的环境变量
//! 5. cordis.patch.yml 行 config
//! 6. settings.yaml 的 namespace                   ← 最高（用户设置页）
//! ```
//!
//! 本模块写**第 6 层**（用户可见可改），且遵循「只填空缺」：
//! 用户已显式设置过的值**不覆盖**。
//!
//! ## 什么不该下发
//!
//! **个人凭据**必须逐个填，不能下发：
//! - `himarket.username/password` —— 每人自己的开发者账号
//! - `roster.rosterToken` —— 每分身签发
//! - `dsh-matrix.userId/accessToken/owner` —— 每分身自己的 Matrix 账号
//!
//! 这些由引导向导处理（见 `matrix_setup.rs`）。

use std::collections::BTreeMap;
use std::path::Path;

/// 一条环境默认值：settings namespace 下的一个键。
pub struct EnvDefault {
    /// settings namespace（如 `roster`）
    pub namespace: &'static str,
    /// namespace 下的键名（如 `rosterUrl`）
    pub key: &'static str,
    /// 默认值
    pub value: &'static str,
}

/// 全公司统一的环境默认值。
///
/// ⚠️ 只放**环境地址与统一默认**，绝不放个人凭据。
/// 修改这里的值时，同步更新 `docs/插件配置下发设计.md` 的契约表。
pub const ENV_DEFAULTS: &[EnvDefault] = &[
    // ── 花名册（dsh-roster-consumer）──
    // rosterUrl 插件默认空串（设计如此：不硬编码），必须下发
    EnvDefault { namespace: "roster", key: "rosterUrl", value: "http://roster.ai.ict.cmcc" },
    // rosterEnabled 默认 false（按分身开）；此处**不**改成 true——
    // 是否对外暴露"我在做什么"应由本人决定，launcher 不代劳。

    // ── 数字分身（dsh-matrix-agent）──
    // homeserverUrl 是环境地址（统一）；userId/accessToken/owner 是个人凭据（下发不了）
    EnvDefault { namespace: "dsh-matrix", key: "homeserverUrl", value: "https://im-ipm.ict.cmcc" },
    EnvDefault { namespace: "dsh-matrix", key: "provider", value: "codebuddy" },
    EnvDefault { namespace: "dsh-matrix", key: "model", value: "deepseek-v4-flash" },

    // ── 默认模型路由 ──
    EnvDefault { namespace: "agent-default-model", key: "provider", value: "codebuddy" },
    EnvDefault { namespace: "agent-default-model", key: "model", value: "deepseek-v4-flash" },
    EnvDefault { namespace: "agent-default-model", key: "reasoningEffort", value: "off" },

    // ── HiMarket 门户（dsh-himarket）──
    // baseUrl / gatewayUrl 插件已内置新环境默认值（domain.ts），无需下发；
    // 但显式写入让用户在设置页能看到并可改。
    EnvDefault { namespace: "himarket", key: "baseUrl", value: "http://market.ai.ict.cmcc" },
    EnvDefault { namespace: "himarket", key: "gatewayUrl", value: "http://job.ai.ict.cmcc" },
    EnvDefault { namespace: "himarket", key: "adminUsername", value: "admin" },

    // ── 数字分身自动激活（P3，matrix-activation）──
    // launcher 的 activation.rs 从 settings.yaml 的 matrix-activation namespace 读这些地址。
    // 都是环境地址（统一值），非个人凭据，可下发。client_secret 属敏感凭据，
    // 不经这里下发（从环境变量 DSH_TWIN_CLIENT_SECRET 读）。
    EnvDefault { namespace: "matrix-activation", key: "keycloakIssuer", value: "https://auth.ict.cmcc/realms/himarket" },
    EnvDefault { namespace: "matrix-activation", key: "clientId", value: "matrix-twin-activation" },
    EnvDefault { namespace: "matrix-activation", key: "activateEndpoint", value: "http://im.ai.ict.cmcc/_matrix/activate" },
    EnvDefault { namespace: "matrix-activation", key: "homeserverUrl", value: "https://im-ipm.ict.cmcc" },
];

/// 应用环境默认值到 settings.yaml（**只填空缺**，不覆盖用户已设的）。
///
/// 返回 (填充数, 跳过数)：填充=原本为空/缺失、已写入；跳过=用户已设、保留。
///
/// 语义与 `clientDefaults` 的「用户显式设置过的不覆盖」一致，但作用域是
/// settings namespace（而非 launcher 自身配置）。
pub fn apply_env_defaults_to_file(path: &Path) -> Result<(usize, usize), String> {
    use serde_yaml::{Mapping, Value};

    let mut root: Mapping = match std::fs::read_to_string(path) {
        Ok(text) => {
            let v: Value = serde_yaml::from_str(&text)
                .map_err(|e| format!("SETTINGS_PARSE_FAILED: {e}"))?;
            v.as_mapping().cloned().unwrap_or_default()
        }
        Err(_) => Mapping::new(), // 文件不存在 → 新建
    };

    let mut filled = 0usize;
    let mut skipped = 0usize;

    for d in ENV_DEFAULTS {
        let section = root
            .entry(Value::String(d.namespace.to_string()))
            .or_insert_with(|| Value::Mapping(Mapping::new()));
        // 若该 namespace 存在但不是 map（用户写坏了），跳过而非报错
        let Some(map) = section.as_mapping_mut() else {
            skipped += 1;
            continue;
        };

        let key = Value::String(d.key.to_string());
        let cur_empty = match map.get(&key) {
            None => true,
            Some(Value::Null) => true,
            Some(Value::String(s)) => s.trim().is_empty(),
            Some(_) => false, // 非字符串值（数字/布尔/列表）视为已设置
        };

        if cur_empty {
            map.insert(key, Value::String(d.value.to_string()));
            filled += 1;
        } else {
            skipped += 1;
        }
    }

    // 什么都没变 → 不写盘（避免无谓的 mtime 变化与 dsh 热重载）
    if filled == 0 {
        return Ok((0, skipped));
    }

    let out = serde_yaml::to_string(&Value::Mapping(root))
        .map_err(|e| format!("SETTINGS_SERIALIZE_FAILED: {e}"))?;
    crate::matrix_setup::atomic_write_public(path, out.as_bytes())?;
    Ok((filled, skipped))
}

/// 把环境默认值整理成 namespace → (key, value) 的表（供 UI/诊断展示）。
pub fn env_defaults_table() -> BTreeMap<&'static str, Vec<(&'static str, &'static str)>> {
    let mut m: BTreeMap<&'static str, Vec<(&'static str, &'static str)>> = BTreeMap::new();
    for d in ENV_DEFAULTS {
        m.entry(d.namespace).or_default().push((d.key, d.value));
    }
    m
}

/// 应用**服务端下发**的环境默认配置到 settings.yaml（仍遵循「只填空缺」）。
///
/// 入参形状：`{ "<namespace>": { "<key>": "<value>" } }`（launcher-server 的
/// `config.json` 的 `envDefaults` 字段）。
///
/// 与 [`apply_env_defaults_to_file`] 的关系：本函数处理**服务端下发**的值，
/// 由 `sync.rs` 在同步时调用；代码内置的 [`ENV_DEFAULTS`] 是兜底（服务端未配时用）。
/// 服务端值优先——管理员可在配置中心改一处、全员生效。
pub fn apply_env_defaults_map_to_file(
    path: &Path,
    server_defaults: &serde_json::Value,
) -> Result<(usize, usize), String> {
    use serde_yaml::{Mapping, Value};

    let Some(obj) = server_defaults.as_object() else {
        return Err("envDefaults 不是对象".to_string());
    };

    let mut root: Mapping = match std::fs::read_to_string(path) {
        Ok(text) => {
            let v: Value = serde_yaml::from_str(&text)
                .map_err(|e| format!("SETTINGS_PARSE_FAILED: {e}"))?;
            v.as_mapping().cloned().unwrap_or_default()
        }
        Err(_) => Mapping::new(),
    };

    let mut filled = 0usize;
    let mut skipped = 0usize;

    for (ns, kv) in obj {
        let Some(kv_obj) = kv.as_object() else {
            continue; // 形状不对（应为 {key: value}）→ 跳过
        };
        let section = root
            .entry(Value::String(ns.clone()))
            .or_insert_with(|| Value::Mapping(Mapping::new()));
        let Some(map) = section.as_mapping_mut() else {
            skipped += kv_obj.len();
            continue;
        };
        for (k, v) in kv_obj {
            // 值统一转字符串（settings.yaml 里这些键都是字符串型）
            let val_str = match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                _ => continue, // 数组/对象不支持下发（避免覆盖复杂结构）
            };
            if val_str.trim().is_empty() {
                continue;
            }
            let key = Value::String(k.clone());
            let cur_empty = match map.get(&key) {
                None => true,
                Some(Value::Null) => true,
                Some(Value::String(s)) => s.trim().is_empty(),
                Some(_) => false,
            };
            if cur_empty {
                map.insert(key, Value::String(val_str));
                filled += 1;
            } else {
                skipped += 1;
            }
        }
    }

    if filled == 0 {
        return Ok((0, skipped));
    }
    let out = serde_yaml::to_string(&Value::Mapping(root))
        .map_err(|e| format!("SETTINGS_SERIALIZE_FAILED: {e}"))?;
    crate::matrix_setup::atomic_write_public(path, out.as_bytes())?;
    Ok((filled, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("dsh-envdef-{name}-{}.yaml", std::process::id()))
    }

    #[test]
    fn fills_missing_keys() {
        let p = tmp_path("fill");
        let _ = std::fs::remove_file(&p);
        let (filled, _) = apply_env_defaults_to_file(&p).unwrap();
        assert_eq!(filled, ENV_DEFAULTS.len());
        let txt = std::fs::read_to_string(&p).unwrap();
        assert!(txt.contains("rosterUrl: http://roster.ai.ict.cmcc"));
        assert!(txt.contains("homeserverUrl: https://im-ipm.ict.cmcc"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn does_not_override_user_value() {
        let p = tmp_path("keep");
        std::fs::write(&p, "roster:\n  rosterUrl: http://my-own:1234\n").unwrap();
        let (_, skipped) = apply_env_defaults_to_file(&p).unwrap();
        assert!(skipped >= 1);
        let txt = std::fs::read_to_string(&p).unwrap();
        assert!(txt.contains("http://my-own:1234"), "用户值必须保留");
        assert!(!txt.contains("roster.ai.ict.cmcc"), "不该写入默认值");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn treats_empty_string_as_unset() {
        let p = tmp_path("empty");
        std::fs::write(&p, "roster:\n  rosterUrl: ''\n").unwrap();
        let (filled, _) = apply_env_defaults_to_file(&p).unwrap();
        assert!(filled >= 1);
        let txt = std::fs::read_to_string(&p).unwrap();
        assert!(txt.contains("roster.ai.ict.cmcc"), "空串应被填充");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn preserves_unrelated_namespaces() {
        let p = tmp_path("keep-other");
        std::fs::write(&p, "dsh-matrix:\n  userId: '@me:im-ipm.ict.cmcc'\n  accessToken: tok\n").unwrap();
        apply_env_defaults_to_file(&p).unwrap();
        let txt = std::fs::read_to_string(&p).unwrap();
        assert!(txt.contains("@me:im-ipm.ict.cmcc"), "个人凭据必须保留");
        assert!(txt.contains("tok"));
        assert!(txt.contains("homeserverUrl: https://im-ipm.ict.cmcc"), "环境地址应填充");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn second_run_is_noop() {
        let p = tmp_path("idempotent");
        let _ = std::fs::remove_file(&p);
        let (f1, _) = apply_env_defaults_to_file(&p).unwrap();
        assert!(f1 > 0);
        let (f2, s2) = apply_env_defaults_to_file(&p).unwrap();
        assert_eq!(f2, 0, "第二次运行不该再填");
        assert_eq!(s2, ENV_DEFAULTS.len());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn server_map_fills_and_keeps_user() {
        let p = tmp_path("servermap");
        std::fs::write(&p, "roster:\n  rosterUrl: http://mine:1\n").unwrap();
        let server = serde_json::json!({
            "roster": { "rosterUrl": "http://server:2", "rosterEnabled": "true" },
            "himarket": { "portalId": "portal-abc" }
        });
        let (filled, skipped) = apply_env_defaults_map_to_file(&p, &server).unwrap();
        assert_eq!(filled, 2, "rosterEnabled 与 portalId 应被填充");
        assert_eq!(skipped, 1, "用户已设的 rosterUrl 应跳过");
        let txt = std::fs::read_to_string(&p).unwrap();
        assert!(txt.contains("http://mine:1"), "用户值必须保留");
        assert!(!txt.contains("http://server:2"), "不该写入被跳过的默认值");
        assert!(txt.contains("portal-abc"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn server_map_ignores_complex_values() {
        let p = tmp_path("servercomplex");
        let _ = std::fs::remove_file(&p);
        // 数组/对象值应被忽略（避免覆盖复杂结构）
        let server = serde_json::json!({
            "llm-codebuddy": { "models": [{"id": "x"}], "baseURL": "http://gw" }
        });
        let (filled, _) = apply_env_defaults_map_to_file(&p, &server).unwrap();
        assert_eq!(filled, 1, "只应填 baseURL");
        let txt = std::fs::read_to_string(&p).unwrap();
        assert!(txt.contains("http://gw"));
        assert!(!txt.contains("models"), "数组值不该被写入");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn server_map_rejects_non_object() {
        let p = tmp_path("serverbad");
        let bad = serde_json::json!("not an object");
        assert!(apply_env_defaults_map_to_file(&p, &bad).is_err());
        let _ = std::fs::remove_file(&p);
    }
}
