# DeepSeek Harness Launcher（rush）

托盘常驻的 DeepSeek Harness 安装 / 启动器（Tauri 2 无窗口应用，仅系统托盘 + 原生通知 + 日志文件）。

## 功能

- **操作反馈中心**：所有用户交互都有可见响应。长操作（安装/修复）自动弹出**操作进度窗口**——
  实时显示步骤列表（✓/⏳/✗）、下载进度百分比、滚动日志；其他操作（启动/同步/测速/上传）托盘菜单
  顶部显示动态状态（`⏳ 安装中：正在下载 Node.js 45%`），完成/失败均有通知与详情。
  托盘「📋 查看进度 / 日志」随时重开窗口。
- **安装 / 修复**：独立安装引擎，一键安装全部依赖（Node.js、pnpm、dsh 核心、Windows 上另有 MinGit），
  多下载源 + Range 断点续传 + SHA-256 校验 + 解压原子切换。Node 已装但版本不符（半成品安装）会重装。
  **dsh 核心走 npm 安装**（`pnpm add @deepseek-ai/dsh@<version>`，npmmirror 稳定源，版本精确可控），
  安装/修复时检测旧 GitHub 结构并自动重建为 npm 结构。
  安装完成后自动创建 web profile 并预置常用插件（`dsh-codebuddy-models`、`dsh-nested-followups`、
  `dsh-plugin-message-rewrite`、`@noob-stupid/dsh-plugin-console`，对齐桌面端）。
- **启动 / 停止 Harness**：`node dsh/lib/bin.js --profile <name> --host 127.0.0.1 --port <port>`，
  Windows 以 `CREATE_NO_WINDOW` 后台运行，退出时 `taskkill /T /F` 回收进程树。
  进程内记录 PID + 端口 + profile 并做存活校验：服务崩溃后状态自动清理，避免「端口已释放但托盘仍显示已运行」；
  重复点击「启动」幂等（已在运行则直接返回现有端口）。
- **多 Profile 切换（托盘子菜单）**：枚举 `<dsh_home>/profiles/` 下的 profile（默认 `matrix` 数字分身，另有 `web` 常规工作环境），
  选中即切换：停止当前 → 以新 profile 启动。默认端口 **3180**（刻意避开桌面端常用的 3080），
  profile 间端口/插件集合完全隔离。
- **数字分身（matrix profile）**：预置时自动创建 `matrix` profile，装 `dsh-matrix-agent`（registry 最新版）
  + `launcher-brand`（自定义品牌名「数字分身」），并挂载内置 `dsh-web-app`。每个分身独立进程/端口，互不冲突。
- **单实例保护**：第二次启动直接退出，不重复拉起 Harness、不互杀进程树。
- **加速设置（托盘子菜单）**：npm 源（自动 / 官方 / npmmirror）与 GitHub 中转（自动 / 直连 / ghfast.top），
  支持多源（JSON 数组按序尝试）；**「测速」一键探测各源延迟**，帮用户选最快的。
  写入配置并同步到 `<dsh_home>/profiles/web/.npmrc`。自动模式用 **IP 地理定位**（ipinfo.io/ip.sb，
  可配置）判定：大陆走镜像，海外直连；IP 检测失败回退系统区域/时区。
- **内置默认配置 + 服务器配置覆盖**：编译内置一份默认配置（端口 3180、profile matrix 等），
  文件缺失也能用；连上服务器后，服务器下发的 `clientDefaults` 自动填充未显式设置的字段
  （用户本地显式写过的字段不被覆盖），管理员可远程统一默认值。
- **常用网址（托盘子菜单）**：可在配置中自定义若干网址，点击用系统浏览器打开。
- **企业同步 + 管理控制台（可选）**：配置 `serverUrl` 后：
  - 定期从中心服务端拉取**插件策略（应装清单）**与**托盘菜单策略**，自动执行——
    缺的插件弹通知 + 托盘一键安装；菜单策略启用时托盘「常用网址」展示管理员下发的项；
  - **推荐插件装到当前生效 profile**（默认 matrix；托盘手动安装同样跟随当前 profile）——
    服务器配置的插件直接出现在同事运行的实例里，不再出现「装进 web 但默认跑 matrix 看不到」；
  - 上报本机完整状态（跨 profile 插件详情、实际菜单、配置、版本）供管理员在服务端查看；
  - **离线完全可用**：连不上服务端时照常使用本地缓存，网络恢复后自动补拉，不影响 dsh web。
- **管理能力（外网代理网关，可选）**：管理员在托盘开启后，本机 127.0.0.1 起本地 API——
  服务端管理页（内网服务器**不能访问外网**）经此中转查询 npm registry 包信息，
  实现「服务端不出网、管理员电脑中转」的隔离架构。HTTP 层用 **tiny_http**（成熟服务器，
  根治自写解析的 body 死锁）；**默认随机 token**（托盘菜单一键复制），CORS 白名单限定管理页域名。
- **镜像上传 + 同步状态（管理页）**：管理员把「应装插件 + 全部依赖」上传到内网 registry
  （`http://registry.ict.cmcc`），管理页显示每个插件「✓ 已同步 / ⚠ 未同步」徽章，可单包/批量同步；
  预发布版本自动加 `--tag next`，上传进度实时显示在操作进度窗口；
  目标 registry 已存在同版本视为已同步（幂等，E409 不再报错）；
  进度轮询带超时与失败重试，管理页刷新后自动恢复显示。
- **launcher 自身自动更新（内网自托管）**：管理员把新版 exe 发布到中心服务端
  （`data/launcher-releases/`，管理页「Launcher 发布」tab 上传），launcher 周期
  （6 小时）轮询 `GET {serverUrl}/api/launcher/latest` 发现新版（版本严格更大）→
  自动下载 → sha256 校验 → 替换自身 exe → 重启。绿色版分发无需重装；
  同内容发布（sha256 一致）自动跳过防循环。`--cmd update-check` 手动检查。
- **CLI / IPC 双通道控制**：`--cmd` 一次性执行（install/launch/stop/sync/speedtest/mirror/status/open-console/test/update-check/update-self），
  IPC 命令供常驻实例调用——自动化测试闭环（`--cmd test` 全流程自测）。
- **数据隔离**：依赖装在自身 AppData 下，`$DSH_HOME` 默认 `~/.dsh-launcher`，与桌面端 `~/.dsh` 互不影响。

## 配置

JSON 文件，格式见 [`launcher-config.json.example`](launcher-config.json.example)。
实际生效位置：`%APPDATA%\io.github.hairyf.deepseek-harness-launcher\launcher-config.json`。

注意：`ghMirrorPrefix` 手编配置时，`"none"`（任意大小写）表示「直连」，空串表示「自动」。

## 企业中心服务端（管理员）

> **已独立成仓**：[`dsh-launcher-center`](https://github.com/evlon/dsh-launcher-center)（Harness 中心管理）。
> 本仓库仅含客户端（托盘）；中心服务端 + 管理页在独立仓库，各自独立发版部署。

中心服务端（`dsh-launcher-center`）负责：向所有客户端分发推荐插件清单、收集各客户端同步状态，
并提供 Web 管理控制台。部署与使用见其仓库 README：

```bash
git clone git@github.com:evlon/dsh-launcher-center.git
cd dsh-launcher-center
node server.js --port 8080 --token 你的管理口令
```

- 管理页：`http://<服务器IP>:8080/admin`（增删推荐插件、查看各客户端同步情况）。
  **登录门禁**：打开页面需输入服务端 `--token`（错误则锁定登录界面，不可绕过）。
- 客户端拉取：`http://<服务器IP>:8080/api/config`（公开端点，客户端无需 token）。
- 生产部署数据（本机 `E:\ai-works\caddy\launcher-data`）保护说明见 `dsh-launcher-center` README。

客户端侧：在 `launcher-config.json` 配置 `serverUrl`（如 `http://10.0.0.5:8080`）即可启用同步；
管理员在中心服务端添加推荐插件后，各客户端下一次轮询（默认 5 分钟）会收到提示并可在托盘确认安装。

### 内网域名（新环境 `*.ai.ict.cmcc`）

内网原有 `*.ict.cmcc`，新部署的 K8S 环境改用 `*.ai.ict.cmcc`。
**2026-09-14 起旧环境已全部下线**（实测旧域名全部 HTTP 000 连接失败），
内置默认与存量配置都已切到新域名。

| 服务 | 新域名（唯一可用） | 旧域名（已下线） |
|---|---|---|
| 中心服务端 `serverUrl` | `http://conf.ai.ict.cmcc` | ~~`http://ai-conf.ict.cmcc`~~ |
| 门户 | `http://market.ai.ict.cmcc` | ~~`http://ai-market.ict.cmcc`~~ |
| 门户管理 | `http://market-admin.ai.ict.cmcc` | ~~`http://ai-market-admin.ict.cmcc`~~ |
| 岗位发布台 | `http://job.ai.ict.cmcc` | ~~`http://ai-job.ict.cmcc`~~ |
| 花名册 | `http://roster.ai.ict.cmcc` | ~~`http://ai-roster.ict.cmcc`~~ |
| 数字人测试台 | `http://test.ai.ict.cmcc` | ~~`http://ai-test.ict.cmcc`~~ |
| 网关控制台 | `http://gateway.ai.ict.cmcc` | ~~`http://ai-gateway.ict.cmcc`~~ |
| 认证（Keycloak） | `http://auth.ai.ict.cmcc` | ~~`http://ai-auth.ict.cmcc`~~ |

**不迁移的域名**（新旧环境共用，实测仍可用）：

- Matrix homeserver `https://im-ipm.ict.cmcc`（承载网 172.21.163.150）
- npm 私服 `http://registry.ict.cmcc`

#### 旧域名自动迁移（0.3.6+）

存量同事的 `launcher-config.json` 里存着已下线的旧域名，而「用户显式设置」的字段
不会被内置默认覆盖 → 升级 launcher 也救不回来。为此启动时**自动迁移**：

- 位置：`launcher-config.json` 的 `serverUrl` / `quickLinks[].url` / `mirrorSettings`
- 行为：改完**立即原子写回**（只改用户真正写过的字段，不固化内置默认值）
- 日志：`域名迁移：serverUrl http://ai-conf.ict.cmcc → http://conf.ai.ict.cmcc`
- 实现：`src-tauri/src/domain_migrate.rs`（含单测，覆盖最长匹配/端口/路径/矩阵与 registry 不迁移）

- **CORS 白名单**：bridge 默认**同时放行新旧两个** `conf` 域名（`http`/`https` 各一），
  无需配置；如需追加其他来源，用环境变量 `ADMIN_ORIGIN`（逗号分隔）。

## 构建与运行

```bash
cd src-tauri
cargo build          # 调试版
cargo build --release  # 发布版
./target/debug/deepseek-harness-launcher.exe
```

## CLI 与 IPC（测试/自动化）

程序支持命令行控制（执行完成后退出）与 IPC 命令（常驻实例可调用），共享执行核心，便于自动化测试：

```bash
# 帮助：-h / --help / help
./deepseek-harness-launcher.exe --help

# CLI：执行命令，全部完成才退出
./deepseek-harness-launcher.exe --cmd status --json   # 查询状态
./deepseek-harness-launcher.exe --cmd install          # 安装/修复
./deepseek-harness-launcher.exe --cmd launch           # 启动 Harness
./deepseek-harness-launcher.exe --cmd stop             # 停止
./deepseek-harness-launcher.exe --cmd sync             # 立即同步
./deepseek-harness-launcher.exe --cmd speedtest        # 测速
./deepseek-harness-launcher.exe --cmd mirror --registry http://registry.ict.cmcc --token xxx   # 镜像上传（等待全部上传完成才退出）
./deepseek-harness-launcher.exe --cmd open-console     # 打开进度窗口
./deepseek-harness-launcher.exe --cmd test             # 全流程自测
```

> `--cmd mirror` 上传在后台线程执行，CLI 会**实时打印进度并等待全部完成才退出**（避免进程提前退出中断上传）。
> 中途 Ctrl+C 可中止；超时上限 31 分钟。

IPC 命令（`invoke('cmd_status')` 等，与 CLI 一一对应）：`cmd_install` / `cmd_launch` / `cmd_stop` / `cmd_sync` / `cmd_speedtest` / `cmd_mirror` / `cmd_status` / `cmd_open_console`。

## 版本号与发布（GitHub Actions）

### 版本号维护在三处，必须逐字相等

| 文件 | 字段 |
|---|---|
| `package.json` | `version` |
| `src-tauri/Cargo.toml` | `[package] version` |
| `src-tauri/tauri.conf.json` | `version` |

**为什么必须一致**：launcher 的自动更新拿**服务端元数据里的版本**与本机
`CARGO_PKG_VERSION`（即 `src-tauri/Cargo.toml` 的 `[package] version`，编译期写死进 exe）
做「**严格大于**」比较（`self_update.rs:79` `has_newer`、`:28` `current_version`）。

- `Cargo.toml` 是**运行时真正生效**的那个 —— 它决定 exe 自报的版本；
- `tauri.conf.json` 的 `version` 是 Tauri 打包时的产品版本；
- `package.json` 的 `version` 是仓库自身声明的版本。

三处不一致时，**发布物 tag / 服务端元数据 / exe 自报版本会互相打架**，典型后果是
**自更新失效且不报错**：`Cargo.toml` 落后 → exe 一直自报旧版、反复下载同一个包；
`Cargo.toml` 超前 → launcher 自认已是最新、**永远收不到新版**。两种都很隐蔽。

改版本号时**三处一起改**，然后本地跑一次护栏自检：

```bash
node scripts/check-version.mjs            # 校验三处一致
node scripts/check-version.mjs v0.3.7     # 再校验 tag 与版本号一致
```

CI 也会跑同一个脚本（见下），**不一致直接构建失败**，不会产出错误版本的安装包。

### 打 tag 发布

打 tag（`v` + 版本号，如 `v0.3.7`）自动触发 `.github/workflows/release.yml`
构建并发布 GitHub Release：

```bash
git tag v0.3.7
git push origin v0.3.7
```

> ⚠️ **tag 必须等于 `v` + 三处版本号**。CI 在 checkout 之后、Rust 工具链与编译**之前**
> 就跑版本护栏，tag 与版本号不符会**立即失败**（省掉一次几分钟的无用构建）。

### ⚠️ 历史 tag 空洞（**只作记录，不补打**）

本仓库已打的 tag：`v0.2.0`、`v0.2.1`、`v0.3.1`、`v0.3.4`、`v0.3.5`、`v0.3.6`、`v0.3.7`。

**`v0.3.0` / `v0.3.2` / `v0.3.3` 这三个版本号从未打过 tag** ——
当时的发布是把 exe 直接传到中心服务端（`conf.…/api/launcher/releases`），
**没有走 GitHub tag 流程**，所以 tag 序列是断的。

**处理决定：这些历史空洞不补打 tag。**

- 补打 tag 会触发 CI 重新构建并发布 GitHub Release，产出的是**今天源码**编出的包，
  而**不是**当年那个 0.3.3 —— 版本号相同、内容不同，**反而制造假证据**；
- 同事真正下载的是**中心服务端**的 exe（`/api/launcher/latest`），**不依赖 GitHub tag**，
  补 tag 对交付没有任何作用；
- 因此：**历史 tag 空洞只在此记录成因，不再回溯**。

**往前看**：自本护栏加入起，每次发版都走 tag 流程，且 tag 与三处版本号由 CI 强制校验，
**不会再产生新的空洞**。

Release 产物为客户端包：

- **客户端包** `deepseek-harness-launcher-client-windows-x64.zip`：主程序 exe + `launcher-brand/` + 配置示例 + 使用说明（面向同事，见 `client-README.md`）

> 中心服务端已拆到独立仓库 [`dsh-launcher-center`](https://github.com/evlon/dsh-launcher-center)，不再随本仓库发布。

运行单元测试：

```bash
cd src-tauri
cargo test
```

> 编译产物在**本项目的** `src-tauri/target/` 目录，与桌面端 `deepseek-harness-desktop` 完全隔离。
> 从零冷编译 `windows`/`webview2-com-sys`/`tao` 等巨型 crate 较占内存，内存不足时可
> 用 `cargo build -j 1` 串行构建降低峰值。
