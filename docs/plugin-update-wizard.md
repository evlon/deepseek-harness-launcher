# 插件更新流程改造（Q1/Q2/Q3）

> 起因：用户报告三个问题 ——
> ① 点「更新插件」成功后**没有任何提示**；
> ② 更新后**还要手点两次**（停止 + 启动）才能生效；
> ③ 整个更新过程**看不到在做什么**。
>
> 决策（用户拍板）：**Q1 = B（更新流程重做，不是只补提示）**、**Q2 = 修（pnpm 失败要真修）**、
> **Q3 = 自动（更新后自动重启 Harness）**。

---

## 一、先纠正一个前提：那次更新其实**失败了**

用户以为「更新成功但没提示」。查真实日志（`launcher.log`）还原出：

```
12:03:23  点「更新 dsh-himarket」
12:03:24  同步完成：待安装推荐：dsh-himarket
12:03:34  操作开始：安装插件
12:03:38  ❌ 安装插件 dsh-himarket@0.1.8 失败（exit=1）
          dsh: pnpm failed in profile directory ...\profiles\matrix
12:03:38  ❌ 操作失败：PLUGIN_INSTALL_FAILED
12:03:42  用户手动点「停止 Harness」   ← 用户在自行摸索
12:03:49  用户手动点「启动 Harness」
12:04:30  启动完成
```

**所以三个现象是同一个根因链**：失败被静默吞掉 → 用户以为成功 → 自己摸索着重启。
这不是「提示不够」的体验问题，是**错误不可见**的功能缺陷。

### 关键对照证据：窗口开没开

同一台机器上，06:55 那次安装**成功**、12:03 那次**失败**，差别在窗口：

| 时刻 | `console://` 轮询次数 | 结果 |
|---|---|---|
| 06:50–06:57（成功那次） | **266 次**（窗口开着） | ✅ 成功，用户全程可见 |
| 12:00–12:05（失败那次） | **0 次**（窗口没开） | ❌ 失败，用户什么都不知道 |

**根因确认**：37 个托盘分支里只有 `op-view` 会 `open_console`，`install`（安装/修复）会弹窗，
但 **`sync-install-*`（插件更新，就是用户点的那个）不弹窗**。

---

## 二、Q2 真根因：pnpm 的错误打在 **stdout**，代码只读 **stderr**

这是本轮最有价值的发现，用**本机实测**证实（不是推测）。

### 复现

用 launcher **自带**的 pnpm 11.7.0，在隔离目录里装一个刚发布的包：

```bash
$ node <launcher>/dependencies/pnpm/bin/pnpm.cjs add dsh-himarket@0.1.8
EXIT=1

# ── STDOUT ──
[ERR_PNPM_NO_MATURE_MATCHING_VERSION] 1 version does not meet the minimumReleaseAge constraint:
  dsh-himarket@0.1.8 was published at 2026-09-21T04:01:02.914Z,
  within the minimumReleaseAge cutoff (2026-09-14T05:26:46.181Z)

# ── STDERR ──
（空）
```

而 `dsh` 的包装逻辑（`@deepseek-ai/dsh/lib/plugin-*.js:124`）只在 **stderr** 写：

```js
process.stderr.write(`${NAME}: pnpm failed in profile directory ${dir}\n`);
```

### 旧代码因此踩了**两个叠在一起的坑**

```rust
// 旧实现（sync.rs）
let stderr = String::from_utf8_lossy(&output.stderr);   // ← 坑①：只读 stderr
if stderr.contains("ERR_PNPM_MINIMUM_RELEASE_AGE_VIOLATION") {   // ← 坑②：错误码名不对
    // 自动加豁免并重试
}
```

| # | 坑 | 后果 |
|---|---|---|
| ① | 真实错误在 stdout，只读 stderr | 用户只看到「详情见日志」，**真因整条丢弃** |
| ② | pnpm 11.7 报的是 `NO_MATURE_MATCHING_VERSION`，不是 `..._VIOLATION` | **自动豁免重试从未触发过** |

**两个条件同时不成立** ⇒ 一个本可自动恢复的场景，变成了用户必须找管理员的失败。

### 修复

- `extract_pnpm_detail(stdout, stderr)`：两个流都读，优先取 `ERR_PNPM_*` 错误码行，
  过滤 `Progress:`/`[WARN]`/`dsh:` 等噪声，按**字符边界**截断 300 字节。
- `is_min_release_age_error(output)`：同时匹配新旧两种错误码，并对
  `minimumReleaseAge` + `cutoff`/`constraint` 做兜底匹配。
- 重试条件改用**合并后的输出**，失败时把真实错误码**透传进 `result`**（不再说「详情见日志」）。

### 端到端验证（真实 pnpm）

```
第 1 次（minimumReleaseAge=7天）        → EXIT=1  [ERR_PNPM_NO_MATURE_MATCHING_VERSION]
按 launcher 逻辑追加 minimumReleaseAgeExclude
第 2 次重试                             → EXIT=0  + dsh-himarket 0.1.8  ✅
```

---

## 三、Q1/Q3 实现：把「一个动作」变成「一个可完成的流程」

### 三个改动

**① 更新走向导窗口**（复用既有 `console.rs`，不新造窗口）

```
旧：点更新 → 黑盒 → 什么都没有
新：点更新 → 向导窗口打开
      ├─ ① 「将要执行」区：dsh-himarket：0.1.7 → 0.1.8 / 目标 profile / 会否重启
      ├─ ② 步骤区：逐步 ✓ 安装插件 → ⏳ 重启 Harness 使其生效
      ├─ ③ 结论区：✓ 完成：dsh-himarket 已就绪，Harness 已自动重启生效
      └─ ④ 历史区（折叠）：事后可回看每次操作的成败、版本、时间
```

**② 操作历史（不再单槽覆盖）**

旧实现只有一个 `static CURRENT`，**新操作直接覆盖旧的** —— 这就是为什么
用户事后想回看「刚才更新成没成」时，`ops-state.json` 里只剩最后一次 `launch`。

改为 `CURRENT` + `HISTORY`（上限 20 条，最新在前），落盘格式：

```json
{ "current": {...}, "history": [ {...}, ... ] }
```

**③ 更新后自动重启**（Q3 = 自动）

```rust
// 语义（不替用户做多余决定）
Harness 本来在运行  → 停止 + 重启（新版本需重载才生效）
Harness 本来没运行  → 不动它（不擅自启动）
重启失败            → 返回 false，如实告知 + 引导手动启动（不谎报「已就绪」）
```

### ⚠️ 向后兼容的一个真 bug（测试抓到）

第一版用「反序列化失败就回退旧格式」：

```rust
match serde_json::from_str::<PersistedState>(&text) {
    Ok(s) => ...,                       // ← 旧格式会「成功」走到这里
    Err(_) => /* 回退单对象 */
}
```

**这永远不会回退**：旧格式的对象没有 `current`/`history` 键，而 serde 默认忽略未知字段、
缺失字段又有 `#[serde(default)]` 兜底 —— 于是旧文件被解析成 `{current: None, history: []}`，
**用户上次的操作结果被静默丢弃**。

改为**显式按字段判别**（`parse_state`）：有 `current`/`history` 键才按新格式，否则按单对象。
单测 `legacy_single_object_still_loads` 钉住这个行为。

---

## 四、验收（全部实测，非推断）

| 层 | 手段 | 结果 |
|---|---|---|
| 单元测试 | `cargo test --release` | **173 passed / 0 failed**（新增 21 项） |
| 向导渲染 | `node scripts/verify-update-wizard.cjs`（最小 DOM stub 真跑内嵌 JS） | **27 passed / 0 failed** |
| 二进制内容 | 在 exe 内检索新 UI 文案与 `/history` 路由 | 全部命中 |
| 真实 exe | 隔离 `DSH_HOME` 跑 `--cmd sync` | EXIT=0，行为正常 |
| Q2 真因 | 真实 pnpm 11.7 复现 + 豁免后重试 | EXIT=1 → EXIT=0 ✅ |
| 注入防护 | 外部文本含 `</script>` | 被转义，不破坏页面 |

### 为什么要有「渲染验证」这一层

「字符串进了二进制」≠「界面真的会显示」。`verify-update-wizard.cjs` 用最小 DOM stub
**真实执行** `console.rs` 内嵌的 JS，断言计划区/步骤/历史/失败结论**确实渲染**。
没有这层，语法错误导致的白屏会溜过去。

---

## 五、现场恢复（我自己造成的影响）

排查过程中我改动了生产 profile，**已逐字节还原**：

| 项 | 处理 |
|---|---|
| `profiles/matrix`（复现时装了 0.1.8） | 用备份覆盖，`diff -r` **完全一致**（回到 0.1.7） |
| `ops-state.json`（`cargo test` 写脏） | 已还原为原始 `launch` 记录 |
| 备份目录放在 `profiles/` 下 | ⚠️ 会被 `list_profiles` 当成新 profile，**已移出**到 Temp |
| 临时测试目录 | 已清理 |

### ⚠️ 顺带修掉一个测试污染隐患

`cargo test` 会写**用户真实的** `~/.dsh-launcher/ops-state.json` ——
因为测试里 `load_cached()` 返回默认配置（`dsh_home = None`），`dsh_home()` 回落到真实目录。

**本轮真实踩到**（生产 `ops-state.json` 被测试内容覆盖）。已修：
`serial_lock()` 自动把落盘路径重定向到临时文件，并加 md5 前后比对验证：

```
测试前 md5 = af68be43...   测试后 md5 = af68be43...   ✓ 生产状态未被触碰
```

---

## 六、改动文件

| 文件 | 改动 |
|---|---|
| `src-tauri/src/ops.rs` | 历史列表、`details`/`started_at`/`finished_at`、`parse_state` 兼容、测试隔离 |
| `src-tauri/src/sync.rs` | `extract_pnpm_detail`、`is_min_release_age_error`、错误透传 |
| `src-tauri/src/tray.rs` | `sync-install-*` 走向导、`pending_entry_at`、`auto_restart_harness` |
| `src-tauri/src/console.rs` | 向导 UI（计划区/历史区）、`/history` 轮询 |
| `src-tauri/src/main.rs` | `console:///history` 路由 |
| `scripts/verify-update-wizard.cjs` | 新增：向导渲染验证 |
| `.gitignore` | 忽略备用 cargo target 目录 |

---

## 七、待办

1. **未发版** —— 改动在工作区（6 个文件），同事要拿到需走发版链。
2. **真人视觉确认未做** —— 渲染层已用 DOM stub 验过，但窗口在**真实屏幕上的观感**
   （尺寸 440×600 是否够放下计划区 + 历史区）建议人工看一眼。
3. **3090/3080 未触碰**；3180（launcher 自己的实例）仍在运行、HTTP 正常。
