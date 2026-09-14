# 排障：提示"安装成功"但无法启动（v0.3.6 修复）

> 2026-09-14 同事实测故障。三处独立根因，均已修复并端到端验证。

## 症状

- 点「安装 / 修复」→ 进度窗口显示全部完成（"DeepSeek Harness 及依赖已就绪"）
- 点「启动 Harness」→ 报 `DSH_NOT_FOUND: 尚未安装 Harness 核心，请先「安装 / 修复」`
- 反复「安装 / 修复」再启动，仍然失败

## 根因 1（最关键）：pnpm 链接被 rename 弄坏

### 现象

安装日志明确显示成功：

```
INFO: pnpm install 输出：Done in 25.6s using pnpm v7.0
INFO: dsh 0.1.5-rc.1 安装完成：C:\Users\xingz\...\dependencies\dsh
INFO: 组件安装完成：Harness 核心
```

但紧接着启动就找不到 `bin.js`。

### 原因

旧实现是「先装到 staging 目录，再 rename 成正式目录」：

```
dependencies/.dsh.installing-1372/   ← 装这里
        ↓ rename
dependencies/dsh/                    ← 变成这里
```

而 **pnpm 在 `node_modules` 里建的是 junction/symlink，且目标写的是绝对路径**：

```
node_modules/@deepseek-ai/dsh
    → C:\...\dependencies\.dsh.installing-1372\node_modules\.pnpm\@deepseek-ai+dsh@x\...\dsh
```

rename 之后，链接仍指向**已不存在的旧路径**。而 `bin.js` 校验是在 rename **之前**做的，
所以日志显示"安装完成"，rename 之后 `dependencies/dsh/node_modules/.../lib/bin.js` 全部不可达
→ `launch` 报 `DSH_NOT_FOUND`。

### 本机复现（决定性）

```
staging 内可达: True  链接类型: Junction
rename 后可达: False  → ❌ 失效（复现同事故障）
```

### 修复

改为**原地安装**：旧目录先 rename 成备份让位 → 在正式路径安装 → 成功后删备份；
失败则回滚（删半成品、把备份挪回）。见 `src-tauri/src/dsh_npm.rs` 的 `install_to` / `rollback_install`。

## 根因 2：launcher-brand 用编译期路径

### 现象

```
WARN: launcher-brand 插件源缺失：E:\ai-works\deepseek-harness-launcher\launcher-brand
WARN: launcher-brand 未就绪，跳过 matrix profile 预置
```

`E:\ai-works\...` 是**开发者机器的路径**，同事机器上根本不存在。

### 原因

用 `env!("CARGO_MANIFEST_DIR")` 定位插件源目录 —— 这是**编译期**路径，会被烧进二进制。
且 launcher 支持自动更新，而更新下载的是**单个 exe**（替换自身），
即使把 `launcher-brand` 放在 zip 里 exe 旁边，更新后该目录也不存在。

### 修复

用 `include_str!` 把 4 个小文件（约 4KB）**内嵌进二进制**，启动时释放到
`<dsh_home>/launcher-brand`。见 `src-tauri/src/install.rs` 的 `materialize_launcher_brand`。
加了回归测试断言"内嵌内容不含 `ai-works`"。

## 根因 3：旧域名已下线，存量配置不会自动迁移

### 现象

```
WARN: 同步失败（离线？）：FETCH_CONFIG_HTTP_502 Bad Gateway；使用缓存配置
INFO: 同步循环启动：间隔 300s，服务端 http://ai-conf.ict.cmcc
```

### 实测（2026-09-14）

| 域名 | 结果 |
|---|---|
| `ai-conf.ict.cmcc` / `ai-market` / `ai-roster` / `ai-job` / `ai-test` / `ai-auth` | ❌ HTTP 000（已下线） |
| `conf.ai.ict.cmcc` / `market` / `roster` / `job` / `test` / `auth` | ✅ HTTP 200 |
| `im-ipm.ict.cmcc`（Matrix） | ✅ 仍可用（承载网，**不迁移**） |
| `registry.ict.cmcc`（npm 私服） | ✅ 仍可用（**不迁移**） |

存量同事的 `launcher-config.json` 里存着旧域名，而**「用户显式设置」的字段不会被内置默认覆盖**
→ 升级 launcher 也救不回来。

### 修复

新增 `src-tauri/src/domain_migrate.rs`：启动时在 **JSON 层**迁移
`serverUrl` / `quickLinks[].url` / `mirrorSettings`，并**原子写回**。

为什么在 JSON 层而不是改完 `LauncherConfig` 再 `save_config`：
后者会把**内置默认值全部固化进用户文件**，导致以后升级 launcher 时新的内置默认再也覆盖不进来。

## 附带修复

| 问题 | 影响 | 修复 |
|---|---|---|
| `installed_dsh_version` 读错清单 | 读的是 launcher 生成的包装清单（`name=dsh-runtime`，**无 version**）→ 版本恒空；且 `--no-open` 判定恒 false → **每次启动都弹浏览器** | 改读 `node_modules/@deepseek-ai/dsh/package.json` |
| `DSH_HOME` 被父进程环境带偏 | launcher 继承父进程环境，若 `DSH_HOME` 指向桌面端 `~/.dsh`，会往用户的**开发环境**里写数据（本人实测污染过 `~/.dsh`） | 增加护栏：指向 `~/.dsh` 时忽略并回落 `~/.dsh-launcher`（显式指定的其它路径仍生效） |

## 验证

- 单测：**103 passed / 0 failed**（新增 domain_migrate 9 项、DSH_HOME 护栏 2 项、launcher-brand 内嵌 2 项）
- 端到端闭环：`install` → `bin.js` 可达 → **`launch` 成功**（端口就绪、HTTP 200）
- 域名迁移：旧域名配置启动后自动改为新域名，且**幂等**（第二次无迁移动作）
- Defender：CustomScan + MOTW 模拟浏览器下载 → 无检测

## 同事如何恢复

1. 下载 **0.3.6**：http://conf.ai.ict.cmcc/download
2. 跑一次「安装 / 修复」（会重建 `dependencies/dsh`，修好链接）
3. 点「启动 Harness」

> 0.3.6 之前的版本即使重装也会再次踩到根因 1，**必须升级**。

## 排查用命令

```powershell
# 看 bin.js 是否可达（根因 1 的直接判据）
Test-Path "$env:APPDATA\io.github.hairyf.deepseek-harness-launcher\dependencies\dsh\node_modules\@deepseek-ai\dsh\lib\bin.js"

# 看链接指向哪里（应指向 dependencies/dsh 下，而非 .installing-* 残留）
(Get-Item "$env:APPDATA\io.github.hairyf.deepseek-harness-launcher\dependencies\dsh\node_modules\@deepseek-ai\dsh" -Force).Target

# 看是否还有 .installing-* / .replaced-* 残留
Get-ChildItem "$env:APPDATA\io.github.hairyf.deepseek-harness-launcher\dependencies" -Force -Directory | Where-Object Name -like '.*'

# 看域名迁移日志
Select-String -Path "$env:APPDATA\io.github.hairyf.deepseek-harness-launcher\logs\launcher.log" -Pattern '域名迁移|指向桌面端目录'

# 排障日志包（发给管理员）
& "$env:APPDATA\..\..\deepseek-harness-launcher.exe" --cmd collect-logs
```
