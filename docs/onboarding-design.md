# Launcher 首次引导激活数字人：填 2 个账号 + 清理重测功能（方案设计）

> 状态：已评审通过（2026-09-07）。本期**只做设计**，实施另立项。
> 适用仓库：deepseek-harness-launcher（客户端托盘）+ 关联 dsh-himarket / dsh-himarket-gateway / dsh-matrix-agent（dsh-bridge）。

## 一、目标与成功标准

**目标**：完全没接触过 DeepSeek 的新同事（小白），双击启动 launcher 后，被引导填入账号信息，数字人（matrix profile 的 Matrix 桥 + HiMarket 市场）激活可用；并能一键"清理"回未激活态，供测试反复验证引导流程。

**小白视角的理想动线**：
```
双击 launcher → 托盘出现（首次/未激活时弹提示）
  → 点击「首次激活向导」或自动弹窗
  → 第 1 步：填 Matrix 分身账号（homeserver 地址 + 分身 userId + 密码）
       launcher 自动调 Matrix login API 换取 accessToken 并验证
  → 第 2 步：填 HiMarket 账号（用户名 + 密码；baseUrl 预置/默认）
       launcher 调 /developers/login 验证并缓存 token
  → 完成：写入配置 → 重启 matrix profile → 通知「数字人已激活 ✓」
  → 使用：在 Matrix 客户端 @ 分身；任务/岗位走 HiMarket
```

**成功标准（可验证）**：
1. 未激活 launcher 启动 → 检测到缺账号 → 弹出/可找到「激活向导」入口，且托盘有明确状态提示（"数字人待激活"）
2. 引导窗口两步填表，字段有说明（小白看得懂），校验即时反馈（Matrix 登录失败/账号密码错有明确报错）
3. 填完保存 → 配置落盘（settings.yaml）→ matrix profile 重启后数字人真正上线（日志无 pending-config/disabled，Matrix 桥已连接）
4. HiMarket 账号保存 → himarket 插件可用（同步/安装技能不报未登录）
5. 「清理 / 重置」功能 → 一键删掉两处账号配置 → 回到"待激活"状态 → 重启 launcher 可再次走完整引导
6. 管理员服务器下发的字段（homeserverUrl / 分身 userId / himarket baseUrl）与用户手填字段不冲突，预置优先、用户补充

## 二、现状事实（调研结论，非猜测）

### 2.1 matrix profile 是"纯 Matrix 桥、无 web UI"
- install.rs 注释明确：matrix profile 无 dsh-web-app（纯数字分身进程），**没有浏览器设置页**——小白无处填账号，必须由 launcher 引导代写配置。
- 预置流程（`preset_matrix_profile`）：装 dsh-matrix-agent + launcher-brand + 写品牌 patch。**不写账号配置**。

### 2.2 dsh-matrix-agent 的账号配置有三条路径
| 路径 | 文件 | 特点 |
|---|---|---|
| bundle 层 patch | profile/node_modules/dsh-matrix-agent/cordis.patch.yml | 随 npm 分发，含 homeserverUrl/userId/owner（企业预置模板），accessToken 留空走环境变量 |
| profile 层 patch | `~/.dsh-launcher/profiles/<p>/cordis.patch.yml` | launcher 维护，可覆盖 config；install.rs 补缺时写 pending-config 占位 |
| **settings 用户层** | **`<DSH_HOME>/settings.yaml`**（= `~/.dsh-launcher/settings.yaml`） | dsh-settings-file 提供；`dsh-matrix:` section 用户层；**chokidar watcher 热加载，外部写文件自动生效** |

**关键结论**：launcher 引导写入 `settings.yaml` 的 `dsh-matrix` section（用户层 > yml config 优先级）即可生效；settings-file 有 watcher，甚至不必重启（连接类字段如 accessToken 需重启——见下）。

### 2.3 三要素与"需重启"字段
- dsh-matrix-agent apply 校验：`homeserverUrl` + `userId` + `accessToken`（或环境变量 DSH_MATRIX_TOKEN）缺一即禁用 Matrix 桥（插件存活、设置可配）。
- settings.ts `RESTART_KEYS`：homeserverUrl / accessToken / userId / digitalTwinMode 等**改动需重启才生效** → 引导完成后需重启 matrix profile 进程。
- 配置完整性变化有 `onConfigChange` 回调可动态起停桥（从缺到全自动连），但 launcher 侧无法触达该运行时回调——最稳妥 = 写配置后重启 profile。

### 2.4 dsh-himarket 账号
- settings namespace = `himarket`，字段：baseUrl / username / password / token（缓存 JWT）/ adminToken 等。
- 落盘同为 settings.yaml 的 `himarket:` section（watcher 热加载，保存即生效，无需重启——himarket 只消费 settings）。
- 生产同事装岗位走 himarket（himarket_install_skill），账号=开发者账号。

### 2.5 Matrix accessToken 获取机制
- dsh-channel-matrix 是**零依赖 fetch 客户端**，没有登录 helper——但 Matrix 标准登录 API 简单：
  `POST {homeserver}/_matrix/client/v3/login` body `{"type":"m.login.password","identifier":{"type":"m.id.user","user":"@ai-x:server"},"password":"..."}` → 返回 `{access_token, user_id}`。
- launcher 已有 reqwest（Rust），引导时可直接调此 API 验证密码并换 token。

### 2.6 账号信息在企业部署中的分布（引导表单字段设计依据）
- **homeserverUrl / 分身 userId / himarket baseUrl**：企业统一值，管理员可通过服务器 clientDefaults / 预置 profile patch 下发——小白不应手填服务器地址。
- **Matrix 密码、himarket 密码**：私有，必须小白填。
- **owner（真人主人 userId）**：dsh-matrix-agent 配置需要（审批/授权应答人），通常 = 当前用户真实账号——需确认是否由管理员预置或引导填。

## 三、引导功能设计

### 3.1 激活状态检测（判定"是否需引导"）
新增 `onboarding.rs`（或并入 config.rs），判定函数 `activation_status(app, cfg) -> OnboardingStatus`：

```rust
enum OnboardingStatus {
    /// 未安装/未预置（提示先「安装 / 修复」）
    NotInstalled,
    /// matrix profile 已装但缺 Matrix 连接参数（homeserverUrl/userId/accessToken 任一缺失或 pending-config）
    NeedsMatrix,
    /// Matrix 已配但 himarket 未配（username/password 空）
    NeedsHimarket,
    /// 两者齐全 → 已激活
    Active,
}
```

**检测来源**（读 settings.yaml 的 `dsh-matrix` / `himarket` section + profile patch 兜底）：
- 启动时读 settings.yaml（若存在），解析两个 namespace 的字段（accessToken/userId/homeserverUrl 非空？username/password 非空？）
- 考虑 token 为空但 DSH_MATRIX_TOKEN 环境变量存在的场景（读 env 判定）
- 结果缓存 + 托盘菜单/首次弹窗读取

### 3.2 引导 UI 载体
复用 launcher 现有 **Tauri WebviewWindow**（与 console.rs 同机制：无窗口应用动态创建窗口 + 自定义协议内嵌 HTML，无需前端构建）。新增窗口 `onboarding`：
- 尺寸 ~460×560，标题「数字人激活向导」
- 本地 HTML（Rust 字符串或 console 同款协议），纯表单 UI，无外部依赖
- **跨进程交互**：窗口 JS → 自定义协议 + HTTP 轮询/提交模式：窗口加载 `http://onboarding.localhost/index.html`，协议 handler 在 Rust 侧提供 `/state`(GET 当前激活状态/预置值)、`/submit`(POST 表单)、`/verify-matrix`(POST 调 Matrix login)、`/verify-himarket`(POST 调 himarket login)。

（实施细节：main.rs 注册 `onboarding` 协议，类似 console 的 register_uri_scheme_protocol；表单值 POST 回 Rust 校验落盘。）

### 3.3 两步表单字段与校验逻辑

**Step 1 — Matrix 分身账号**（预置已提供的字段显示为只读/可改）：
| 字段 | 来源 | 交互 |
|---|---|---|
| 服务器地址 homeserverUrl | 预置（profile patch / 服务器下发）| 只读展示，可展开修改 |
| 分身账号 userId | 预置 | 只读展示（小白不改分身名）|
| 密码 | 小白填 | 密码框，点「验证并继续」→ POST 本机 API → Rust reqwest 调 Matrix /login → 成功返回 accessToken（不进 UI，直接落内存待写）|

校验失败反馈：401/403 →「分身账号或密码不对」；网络错误 →「连不上服务器，检查地址或网络」。

**Step 2 — HiMarket 账号**：
| 字段 | 来源 | 交互 |
|---|---|---|
| 市场地址 baseUrl | 预置默认（http://ai-market.ict.cmcc）| 只读，可改 |
| 用户名 | 小白填 | 文本框 |
| 密码 | 小白填 | 密码框，「验证并继续」→ POST /developers/login → 缓存 token |

**Step 3 — 完成确认**：展示将写入的两项（Matrix userId / himarket username），点「完成并启动」→ Rust：
1. 把 Matrix accessToken + userId + homeserverUrl 写入 settings.yaml `dsh-matrix` section（merge，保注释）
2. 把 himarket baseUrl/username/password/token 写入 settings.yaml `himarket` section
3. 重启 matrix profile（workflow::stop + launch，或仅重启 dsh 进程）
4. 通知「数字人已激活」+ 打开提示（在 Matrix 客户端 @ 分身试试）

### 3.4 落盘实现（写入 settings.yaml）
新增 `settings_yaml.rs`（或并入 onboarding.rs）：
- 读 `~/.dsh-launcher/settings.yaml`（YAML 文本，保留未涉及 section 原样 + 注释——参照 dsh-settings-file 的 comment-preserving 精神；若文件不存在则新建 `dsh-matrix:` / `himarket:` 两个 section）
- 更新目标 section 的**叶子字段**（不动同 section 其它镜像字段如 timelineSnapshot——注意 settings.yaml 里 dsh-matrix section 已有大量运行时镜像数据，必须 merge 而非整节覆盖！）
- 写入临时文件 + 原子替换（防 dsh 进程并发读写冲突；可参考 dsh-atomic-write 思路）
- **陷阱**：settings.yaml 的 dsh-matrix section 含 timelineSnapshot/tasksSnapshot/ownerInbox 等运行时大块镜像——launcher 写入时绝不能清掉这些，必须逐字段 set。
  → 稳妥做法：读入 → 解析成对象 → 仅覆盖账号键（homeserverUrl/accessToken/userId/owner）→ 其余键原样 → 序列化写回。YAML 注释可能丢失，可接受（这些是用户数据非注释关键）；或最小化 diff。

### 3.5 托盘与首启入口
- 托盘菜单「同步 / 推荐插件」区上方或新增状态行：未激活 → `⚠️ 数字人待激活（点击开始向导）`；已激活 → 无额外行（或「数字人已激活」灰置）
- 首次启动（检测到 NeedsMatrix）→ 自动弹向导窗口 + 通知
- 「激活向导」菜单项：随时可重开
- 完成激活后 `refresh_sync_menu`

### 3.6 安全
- 密码仅在表单 → 本机 API 传输（127.0.0.1）→ 换取 token；**token 明文落 settings.yaml**（与 dsh-matrix-agent/himarket 现状一致——accessToken/密码本就在 settings.yaml 明文；himarket 密码也明文）。不新增额外暴露。
- accessToken 属连接类 RESTART_KEYS，写入后重启生效。

## 四、清理 / 重置功能设计（测试引导用）

### 4.1 入口
- 托盘「激活向导」子菜单内：`🗑 清除账号配置（重置向导）`（灰色危险项，点击需确认通知/二次弹窗确认）
- 或 CLI：`launcher.exe --cmd onboarding-reset`（自动化测试用，README CLI 段可加）

### 4.2 清理动作
1. 停 matrix profile（若运行）
2. 从 settings.yaml **删除** `dsh-matrix` section 的账号键（homeserverUrl/accessToken/userId/owner 置空或删键；**保留** timelineSnapshot 等镜像键避免脏数据；或更彻底：整段删 `dsh-matrix` + `himarket`——但会连运行时镜像一起清。取舍：**账号键置空 + 保留镜像** 为默认，或提供"彻底重置"选项删整节）
3. 删除 `himarket` section 的 username/password/token/baseUrl（保留 skillInstallDir 等非账号键）
4. profile patch 里的 pending-config 占位回归原状（若 launcher 补过）
5. 记录清理完成 → 状态回 NeedsMatrix → 通知「已重置，可重新走激活向导」
6. 重启 profile（让"未激活"生效）或提示用户重启

### 4.3 自动化测试钩子
CLI 增加：
- `--cmd onboarding-status`：输出当前激活状态 JSON（NotInstalled/NeedsMatrix/NeedsHimarket/Active + 缺哪些字段）
- `--cmd onboarding-reset`：执行清理
配合现有 `--cmd launch/stop` 可写端到端测试脚本：reset → 启动 → 断言托盘状态待激活 → 模拟填表（调本机 API 或直接写 settings.yaml）→ 重启 → 断言 Active。

## 五、Matrix 统一认证（未来期，本期仅设计）

**诉求**：小白只填 Matrix 账号（或更少），himarket 凭据由系统换取。

**现状障碍**（调研确认）：
- himarket 开发者账号体系独立（/developers/login，username+password），与 Matrix 账号无任何绑定关系
- himarket-gateway 无 Matrix↔developer 映射；无 "connection 账号" 现成概念
- 用户提到「和 actress 管理员账号沟通获取 connection 账号和密钥」——需先明确 actress 是什么系统/管理员入口（本期未调研到对应物，需用户补充）

**建议架构（分两段）**：
```
段 A（轻，推荐先做）：账号绑定台账 + 换发接口
  新增服务（或扩 himarket-gateway）：
  - 管理员后台维护「Matrix userId ↔ himarket 开发者账号」绑定表
  - 新 API：POST /api/onboarding/exchange
      body { matrixUserId } （可再带 Matrix 短时验证凭据）
      → 校验绑定存在 → 返回 { himarketUsername, oneTimeCredential }
  - 引导窗只填 Matrix 密码 → launcher 调 exchange 拿 himarket 凭据 → 落 settings.yaml
  安全：绑定表管理员维护；oneTimeCredential 短时有效；审计记录

段 B（远期）：Matrix 作为企业统一 IdP
  若企业已有 Matrix 账号即全员身份，himarket 开发者账号改为「Matrix 登录」或由 IdP 下发，
  引导时用 Matrix token 直接调 himarket 换开发者会话——需 HiMarket 侧支持第三方登录或网关代登录
```
**本期建议**：不在本期实现统一认证。本期引导=填 2 组（Matrix 密码 + himarket 账号）；统一认证单独立项（需先明确 actress 管理员接口）。

## 六、涉及改动清单（实施阶段用）

| 子系统 | 文件 | 改动 |
|---|---|---|
| 激活状态 | `src-tauri/src/config.rs` 或新增 `onboarding.rs` | OnboardingStatus 判定（读 settings.yaml + env）|
| settings.yaml 读写 | 新增 `src-tauri/src/settings_yaml.rs` | YAML 解析（serde_yaml 或手写最小解析——注意 Cargo.toml 需加依赖或复用现有）、merge 账号键、原子写 |
| Matrix 登录换 token | onboarding.rs 内 | reqwest POST /_matrix/client/v3/login |
| himarket 验证 | onboarding.rs 内 | reqwest POST /api/v1/developers/login |
| 引导窗口 | `main.rs` + 新 `onboarding_console.rs` | 注册 onboarding 自定义协议；HTML 两步表单；/state /submit /verify-* 端点 |
| 托盘 | `tray.rs` | 「激活向导」子菜单 + 待激活状态行 + 清理项 |
| 清理 | onboarding.rs | 账号键置空/删除 + profile patch 还原 + 重启 |
| CLI | `cli.rs` | onboarding-status / onboarding-reset |
| 重启激活 | workflow.rs 调用 | 保存后重启 matrix profile |
| Cargo.toml | 依赖 | 可能加 serde_yaml（确认现有 yaml 依赖）；reqwest 已有 |

## 七、验收测试清单

1. **全新机器模拟**：`--cmd onboarding-reset` → 启动 launcher → 托盘出现「⚠️ 数字人待激活」→ 打开向导
2. **Matrix 步骤**：填错密码 → 明确报错；填对 → 换取 token（日志确认 /login 200）
3. **HiMarket 步骤**：填错账号 → 报错；填对 → 缓存 token
4. **落盘检查**：settings.yaml `dsh-matrix`/`himarket` section 出现账号字段；**timelineSnapshot 等镜像键未被清**
5. **激活验证**：重启 matrix profile → 日志出现 Matrix 桥已连接（无 pending-config / disabled / incomplete config）；himarket 日志无未登录
6. **重复引导**：再次打开向导 → 显示已填值（可改）
7. **清理**：托盘清理 → settings.yaml 账号键消失、镜像键保留 → 重启 → 回到待激活 → 可再走完整引导
8. **自动测试**：CLI 三连（reset → status=NeedsMatrix → 模拟写配置 → status=Active）
9. 管理员预置字段（homeserverUrl/userId/baseUrl）在向导中只读展示不被误改

## 八、风险与假设

- **假设 A**：matrix profile 运行所需 homeserverUrl / 分身 userId / owner 由管理员预置（bundle patch 或服务器下发）——若需小白手填服务器地址，引导表单需加该字段并配默认值提示。
- **假设 B**：owner（真人主人）账号默认 = 用户 himarket/Matrix 当前人，或管理员预置。需在实施前确认归属策略。
- **风险 1**：settings.yaml 并发写（dsh 进程运行中写镜像）→ 原子替换 + 尽量在 profile 停止时写；必要时复用 dsh-atomic-write 的锁思路。
- **风险 2**：YAML 注释/格式破坏 → 逐字段 merge、保留未知键；测试用例覆盖"文件已被 dsh 写满镜像数据"场景。
- **风险 3**：Matrix 登录需确认企业 homeserver 允许 m.login.password 与设备管理策略；token 属于"设备"，重复登录会产生多设备——引导可先 /login 再按需 logout 旧设备。
- **开放问题**：actress 管理员接口（用户提的换 connection 凭据渠道）是什么系统、能否对接——决定未来期统一认证方案，需用户补充信息后再设计。

## 九、实施顺序建议（后续立项）

1. Phase 0：Cargo.toml 依赖确认（yaml 解析）+ onboarding.rs 骨架（状态判定）
2. Phase 1：settings_yaml.rs（读/merge/写 + 单测，含"镜像键保留"测试）
3. Phase 2：Matrix/himarket 登录验证（reqwest + 单测可 mock）
4. Phase 3：引导窗口（协议 + HTML 表单 + /submit）
5. Phase 4：托盘入口 + 状态行 + 清理菜单
6. Phase 5：CLI onboarding-status/reset + 端到端测试
7. Phase 6：真实环境验收（3090/本机 launcher 实测）
