# DeepSeek Harness Launcher（rush）

托盘常驻的 DeepSeek Harness 安装 / 启动器（Tauri 2 无窗口应用，仅系统托盘 + 原生通知 + 日志文件）。

## 功能

- **安装 / 修复**：独立安装引擎，一键安装全部依赖（Node.js、pnpm、dsh 核心、Windows 上另有 MinGit），
  多下载源 + Range 断点续传 + SHA-256 校验 + 解压原子切换。Node 已装但版本不符（半成品安装）会重装。
  安装完成后自动创建 web profile 并预置常用插件（`dsh-codebuddy-models`、`dsh-nested-followups`、
  `dsh-plugin-message-rewrite`、`@noob-stupid/dsh-plugin-console`，对齐桌面端）。
- **启动 / 停止 Harness**：`node dsh/lib/bin.js --profile <name> --host 127.0.0.1 --port <port>`，
  Windows 以 `CREATE_NO_WINDOW` 后台运行，退出时 `taskkill /T /F` 回收进程树。
  进程内记录 PID + 端口 + profile 并做存活校验：服务崩溃后状态自动清理，避免「端口已释放但托盘仍显示已运行」；
  重复点击「启动」幂等（已在运行则直接返回现有端口）。
- **多 Profile 切换（托盘子菜单）**：枚举 `<dsh_home>/profiles/` 下的 profile（默认 `web`，另有 `matrix` 数字分身），
  选中即切换：停止当前 → 以新 profile 启动。默认端口 **3180**（刻意避开桌面端常用的 3080），
  profile 间端口/插件集合完全隔离。
- **数字分身（matrix profile）**：预置时自动创建 `matrix` profile，装 `dsh-matrix-agent`（registry 最新版）
  + `launcher-brand`（自定义品牌名「数字分身」），并挂载内置 `dsh-web-app`。每个分身独立进程/端口，互不冲突。
- **单实例保护**：第二次启动直接退出，不重复拉起 Harness、不互杀进程树。
- **加速设置（托盘子菜单）**：npm 源（自动 / 官方 / npmmirror）与 GitHub 中转（自动 / 直连 / ghfast.top），
  写入配置并同步到 `<dsh_home>/profiles/web/.npmrc`。自动模式用 **IP 地理定位**（ipinfo.io/ip.sb，
  可配置）判定：大陆走镜像，海外直连；IP 检测失败回退系统区域/时区。
- **内置默认配置 + 服务器配置覆盖**：编译内置一份默认配置（端口 3180、profile web 等），
  文件缺失也能用；连上服务器后，服务器下发的 `clientDefaults` 自动填充未显式设置的字段
  （用户本地显式写过的字段不被覆盖），管理员可远程统一默认值。
- **常用网址（托盘子菜单）**：可在配置中自定义若干网址，点击用系统浏览器打开。
- **企业同步 + 管理控制台（可选）**：配置 `serverUrl` 后：
  - 定期从中心服务端拉取**插件策略（应装清单）**与**托盘菜单策略**，自动执行——
    缺的插件弹通知 + 托盘一键安装；菜单策略启用时托盘「常用网址」展示管理员下发的项；
  - 上报本机完整状态（跨 profile 插件详情、实际菜单、配置、版本）供管理员在服务端查看；
  - **离线完全可用**：连不上服务端时照常使用本地缓存，网络恢复后自动补拉，不影响 dsh web。
- **管理能力（外网代理网关，可选）**：管理员在托盘开启后，本机 127.0.0.1 起本地 API——
  服务端管理页（内网服务器**不能访问外网**）经此中转查询 npm registry 包信息，
  实现「服务端不出网、管理员电脑中转」的隔离架构。
- **数据隔离**：依赖装在自身 AppData 下，`$DSH_HOME` 默认 `~/.dsh-launcher`，与桌面端 `~/.dsh` 互不影响。

## 配置

JSON 文件，格式见 [`launcher-config.json.example`](launcher-config.json.example)。
实际生效位置：`%APPDATA%\io.github.hairyf.deepseek-harness-launcher\launcher-config.json`。

注意：`ghMirrorPrefix` 手编配置时，`"none"`（任意大小写）表示「直连」，空串表示「自动」。

## 企业中心服务端（管理员）

`server/` 目录是一个零依赖的 Node 服务，部署在内网服务器上，负责：
向所有客户端分发推荐插件清单、收集各客户端同步状态。

```bash
cd server
node server.js --port 8080 --token 你的管理口令
```

- 管理页：`http://<服务器IP>:8080/admin`（增删推荐插件、查看各客户端同步情况）。
- 客户端拉取：`http://<服务器IP>:8080/api/config`。
- 数据存于 `server/data/`（`config.json` + `clients/<clientId>.json`）。
- 详见 [`server/README.md`](server/README.md)。

客户端侧：在 `launcher-config.json` 配置 `serverUrl`（如 `http://10.0.0.5:8080`）即可启用同步；
管理员在服务端添加推荐插件后，各客户端下一次轮询（默认 5 分钟）会收到提示并可在托盘确认安装。

## 构建与运行

```bash
cd src-tauri
cargo build          # 调试版
cargo build --release  # 发布版
./target/debug/deepseek-harness-launcher.exe
```

运行单元测试：

```bash
cd src-tauri
cargo test
```

> 编译产物在**本项目的** `src-tauri/target/` 目录，与桌面端 `deepseek-harness-desktop` 完全隔离。
> 从零冷编译 `windows`/`webview2-com-sys`/`tao` 等巨型 crate 较占内存，内存不足时可
> 用 `cargo build -j 1` 串行构建降低峰值。
