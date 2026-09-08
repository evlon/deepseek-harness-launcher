# Launcher「数字分身激活向导」设计（Matrix 配置引导 + 进度 + 清理重测）

> 状态：评审通过（2026-09-07，第 2 版——按用户纠正修订）。
> 适用范围：deepseek-harness-launcher（客户端托盘）。
> 本期范围：**只做 dsh-matrix-agent（Matrix 配置）的引导**；HiMarket/其它账号引导另议。
> 目标用户：没接触过 DeepSeek 的新同事（小白）。

## 一、定位（用户澄清后的正确定位）

**向导 = dsh-matrix-agent 的「Matrix 配置引导层」，不是独立认证体系。**

- dsh-matrix-agent 本身有一套设置 UI（settings 页），能配 matrix 配置；问题是 **matrix profile 是纯 Matrix 桥、无 web UI**，小白找不到/不习惯设置页，体验差。
- 向导做的事与设置 UI **等效**：收集 dsh-matrix-agent 的 matrix 配置字段（homeserverUrl / userId / accessToken，可能含 owner 等），写入 dsh-matrix-agent **实际读取的同一配置源**。
- 向导存在的意义 = 把"配置 → 重启 → 验证 → 可用"串成**小白看得懂的分步流程**，输入完一键跑完，而不是让小白自己在设置页里摸索、填完不知道有没有生效。
- 用户原话要点：
  1. "matrix 认证是通过向导启动时配置 dsh-matrix-agent 里面的 matrix 配置" —— 写同一配置源；
  2. "向导里提醒用户配置，只是为小白更易用，因为 dsh-matrix-agent 设置 UI 里也能设置，只是体验不好"；
  3. "向导要达成：运行后用户输入配置信息 → 一步步提示进度 → 最终得到可用的程序"。

## 二、成功标准（小白视角）

```
启动 launcher（未配置 matrix）→ 托盘出现「⚠️ 数字分身待配置」
  → 点「配置数字分身」打开向导
  → 表单：服务器地址 / 分身账号 / accessToken（或 账号+密码自动获取）
  → 点「开始配置」→ 弹出进度窗口，分步走：
       ① 写入配置（settings.yaml dsh-matrix section）
       ② 重启数字分身（matrix profile）
       ③ 等待 Matrix 连接成功
       ④ 完成 ✓（通知：数字分身已可用，在 Matrix 客户端 @ 它试试）
```

1. 未配置时启动 → 托盘有明确状态 + 向导入口
2. 表单字段与 dsh-matrix-agent 设置 UI 同字段同语义；每字段有小白能懂的说明
3. 提交后**一步步可见进度**（复用操作进度窗口），每步成功/失败明确；失败可重试/回看
4. 完成后 matrix 数字分身真正可用（日志无 pending-config / disabled / incomplete config，Matrix 桥已连）
5. 再次打开向导显示已填值（可改）；重复执行幂等
6. 「清理」一键回到未配置态，可反复走完整流程测试

## 三、现状事实（沿用第 1 版调研，均验证过）

| 事实 | 含义 |
|---|---|
| matrix profile 纯 Matrix 桥、无 web UI（install.rs 注释）| 设置 UI 对小白不可达 → 向导价值所在 |
| dsh-matrix-agent 校验 homeserverUrl + userId + accessToken（或 env DSH_MATRIX_TOKEN）| 三要素缺一即禁用桥（插件存活、配置可补）|
| settings 用户层 = `<DSH_HOME>/settings.yaml`（= ~/.dsh-launcher/settings.yaml），chokidar watcher 热加载 | **向导写这里 = 与设置 UI 同一配置源**（dsh-matrix-agent 也注册 dsh-matrix namespace）|
| settings.ts RESTART_KEYS：accessToken/userId/homeserverUrl 改动需重启生效 | 写配置后须重启 matrix profile |
| dsh-matrix section 已含 timelineSnapshot/tasksSnapshot 等运行时镜像数据 | 写必须**逐字段 merge**，绝不整节覆盖 |
| Matrix 标准登录 API：POST {hs}/_matrix/client/v3/login（m.login.password）→ access_token | 提供"账号+密码自动获取 token"便利钮的底层 |
| 连接类配置完整 → 插件 onConfigChange 动态起桥；但 launcher 侧最稳妥 = 重启 profile | 分步执行里第②步重启 |

## 四、向导设计

### 4.1 入口与状态
- 新增激活状态判定（读 settings.yaml `dsh-matrix` section + env）：
  - `MatrixUnconfigured`：homeserverUrl/userId/accessToken 任一缺失/空/pending-config
  - `MatrixConfigured`：三要素齐全（已激活）
- 托盘：未配置 → 顶部状态行 `⚠️ 数字分身待配置` + 菜单项「配置数字分身」；已配置 → 菜单项变「数字分身设置」（可改）
- 首次启动（MatrixUnconfigured）→ 自动弹向导窗口 + 通知

### 4.2 表单（复用 console 同款自定义协议窗口）
窗口：`matrix-setup`，~480×520，标题「配置数字分身」

字段（与 dsh-matrix-agent 设置 UI 对齐）：
| 字段 | 说明（小白向）| 预置/交互 |
|---|---|---|
| homeserverUrl | "数字分身服务器地址（管理员提供）"| 预置可改 |
| userId | "分身账号（如 @ai-zhangsan:server）"| 预置可改 |
| accessToken | "分身访问令牌"| 密码框 + 「用账号密码自动获取」切换 |
| owner（可选）| "主人账号（审批归谁）"| 可空，管理员预置则展示 |

accessToken 获取便利钮：切到「账号+密码」→ 填 userId + 密码 → 点「获取令牌」→ launcher 调 Matrix /login → 成功回填 accessToken 字段（token 本身仍落 settings.yaml，密码不落盘）。

### 4.3 提交 → 分步执行（复用操作进度窗口）
点「开始配置」→ Rust 侧登记操作（op label="配置数字分身"），**自动弹 console 进度窗口**（install 同款），步骤：

1. **写入配置**：settings.yaml `dsh-matrix` section 逐字段 merge（homeserverUrl/userId/accessToken/owner），原子写；保留 timelineSnapshot 等镜像键
2. **重启数字分身**：stop matrix profile（若运行）→ launch；若未运行直接 launch
3. **等待 Matrix 连接**：轮询 dsh-matrix-agent 状态（读诊断日志 stateDir/diagnostics.log 或日志关键词 "Matrix bridge started" / 无 "incomplete config"），超时给失败
4. **完成**：✓ 通知「数字分身已可用，在 Matrix 客户端 @ 它试试」；托盘刷新为已配置

每步经 ops.rs mark_step_running/finish；失败标红、可重试。

### 4.4 清理 / 重置（测试引导用）
- 入口：托盘数字分身菜单内「🗑 清除配置（重置向导）」（二次确认）；CLI `--cmd matrix-setup-reset`
- 动作：停 matrix profile → settings.yaml `dsh-matrix` section 账号键置空（保留镜像键）→ 通知回未配置态
- CLI `--cmd matrix-setup-status`：输出配置状态 JSON，供自动化断言

## 五、涉及改动（实施阶段用）

| 文件 | 改动 |
|---|---|
| 新增 `src-tauri/src/matrix_setup.rs` | 状态判定 / 写 settings.yaml(逐字段 merge) / Matrix login 换取 / 分步执行编排 |
| `main.rs` | mod 声明 + 注册 matrix-setup 自定义协议（仿 console）|
| 新增 `matrix_setup_html`（或并入上）| 表单窗口 HTML + /state /submit /fetch-token 端点 |
| `tray.rs` | 待配置状态行 + 「配置数字分身」菜单 + 清理项 + 完成后 refresh |
| `cli.rs` | matrix-setup-status / matrix-setup-reset |
| `ops.rs`/console | 复用（分步进度展示）|
| Cargo.toml | 若需 YAML 解析加依赖（确认现有）|

## 六、验收清单

1. 全新态启动 → 托盘「⚠️ 数字分身待配置」→ 向导自动弹出
2. 表单填错/空 → 即时提示；accessToken 自动获取：密码错明确报错、成功回填
3. 开始配置 → 进度窗口分步显示 ①写配置 ②重启 ③等连接 ④完成，无跳步
4. 落盘检查：settings.yaml dsh-matrix section 账号键更新、**镜像键保留**；matrix profile 重启无 pending-config/disabled
5. 完成后托盘变已配置；重开向导显示已填值可改；重复执行幂等
6. 清理 → 回未配置态 → 可再走全流程
7. CLI：reset → status=unconfigured → 写配置 → status=configured
