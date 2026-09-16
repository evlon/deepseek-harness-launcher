# 内网根 CA 安装缺陷修复（2026-09-16）

## 背景

内网 `*.ai.ict.cmcc` 域名（`im.ai.ict.cmcc` 等）启用 HTTPS，由自建内网根 CA
`ICT Internal AI Root CA` 签发。浏览器（Chrome/Edge）默认不信任该根 CA，
launcher 必须在首次安装时把它导入 Windows 系统信任库，否则访问会一直红锁。

## 根因（三个缺陷叠加）

| # | 缺陷 | 后果 |
|---|---|---|
| 1 | `install_root_ca` 排在 `install_all` **最末一步** | 前面任何一步失败即 `return Err` 提前退出，证书安装永远执行不到 |
| 2 | 失败只弹通知、**不阻断**、且文案谎称"可在托盘『同步』菜单重试" | 托盘「同步」菜单根本不调用证书安装，用户**无重试入口** |
| 3 | 无验证闭环：`certutil -addstore` 返回非零时**不解析原因** | 用户不知道是权限不足还是别的，装失败也无人察觉 |

## 修复内容

### 1. `src-tauri/src/install.rs`

- **`install_root_ca` 重构**：
  - 由 `fn`（无返回值、失败静默）改为 `pub fn` 返回 `Result<(), String>`；
  - 新增**验证闭环**：`certutil -addstore` 成功后，再 `certutil -store Root` 读回
    确认 `ICT Internal AI Root CA` 确实在「受信任的根证书颁发机构」里；
  - 失败时解析 stderr，精确区分「需要管理员权限」（含 `Access is denied` /
    `拒绝访问` / `0x80070005` / `Administrator` / `管理员` 关键字）与其他错误。
- **`install_all` 调整**：
  - 证书导入从「最后一步」**提前到第一步**（组件下载之前），独立于依赖下载的最快闭环，
    不再被后续步骤短路；
  - 纳入步骤体系：`steps` 数组新增「导入内网根证书」为第 0 步，
    后续组件步骤索引相应 +1（`step_index` 从 0 改 1，硬编码索引 3/4/5 改 4/5/6）；
  - 失败**仍不阻断依赖安装**（依赖本体要装好），但显式通知并引导走托盘重试入口。

### 2. `src-tauri/src/tray.rs`

- 新增独立托盘菜单项 **「🔐 重装内网证书」**（`cert-reinstall`），
  直接调用 `install_root_ca`，成功/失败都有明确通知；失败且需管理员权限时，
  提示「以管理员身份重新运行 launcher 后再试」。
- 修复了「注释说有重试入口、实际没有」的 bug。

### 3. 纳管证书文件

- `src-tauri/resources/ict-internal-ca.crt`（公钥证书，无私钥）此前**未纳入 git**。
  `install.rs` 用 `include_str!("../resources/ict-internal-ca.crt")` 编译期内嵌，
  干净 clone / CI 会因文件缺失编译失败。已 `git add` 纳管。

## 验证

- `cargo check`：通过（无错误、无警告）。
- `cargo build --release`：通过，产出
  `src-tauri/target/release/deepseek-harness-launcher.exe`（17,264,640 B）。

## 遗留

- **Firefox 未处理**：Firefox 用独立 NSS 证书库，`certutil -addstore "Root"`
  只影响系统信任库（Chrome/Edge 读这里）。同事主要用 Chrome/Edge，Firefox 后续可选补。
- **管理员权限提示**：当前提示用户「以管理员身份重新运行 launcher」。
  更优方案（launcher 自提权）未实现——Tauri 2 无内置 UAC 提权 API，
  需引入 `windows` crate 的 `ShellExecuteW` + `runas` verb，属后续增强项。
