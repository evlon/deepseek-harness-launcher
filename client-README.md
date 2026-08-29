# DeepSeek Harness Launcher（客户端）

托盘常驻的 DeepSeek Harness 安装 / 启动器。

## 快速开始

1. **双击运行** `deepseek-harness-launcher.exe`（无需安装，绿色单文件）
2. 程序出现在**系统托盘**（任务栏右下角，可能要点 `^` 展开）
3. 右键托盘图标，点 **「安装 / 修复」**——自动下载并安装全部依赖（Node.js、pnpm、dsh 核心），约几分钟
4. 安装完成后，点 **「启动 Harness」**，浏览器访问 `http://127.0.0.1:3180` 即可使用

> 若公司统一配置了中心服务端（`serverUrl`），安装后会自动同步推荐插件与菜单配置，无需手动设置。

## 托盘菜单说明

| 菜单 | 作用 |
|---|---|
| 安装 / 修复 | 安装或修复全部依赖（首次必点） |
| 启动 Harness | 启动本地服务，浏览器打开 `http://127.0.0.1:3180` |
| 打开 Harness 页面 | 打开已启动的服务页面 |
| 停止 Harness | 停止本地服务 |
| 切换 Profile | 切换不同配置（web / matrix 数字分身等） |
| 管理能力 | 管理员专用：开启后供中心管理页中转查询（普通用户不用管） |
| 加速设置 | 切换 npm 源 / GitHub 中转，可「测速」选最快的 |
| 同步 / 推荐插件 | 显示服务端推荐的待安装插件，一键安装 |
| 常用网址 | 快捷打开常用网站（管理员可统一下发） |
| 查看日志 | 打开日志文件（排障用） |

## 数据隔离

- 依赖装在 `%APPDATA%\io.github.hairyf.deepseek-harness-launcher\`
- Harness 用户数据默认 `~/.dsh-launcher`，与桌面端 `~/.dsh` 互不影响
- 默认端口 **3180**（不与桌面端 3080 冲突）

## 配置（可选）

一般无需手动配置。如需自定义，编辑 `%APPDATA%\io.github.hairyf.deepseek-harness-launcher\launcher-config.json`
（首次运行自动创建，格式见随包 `launcher-config.json.example`）。

## 卸载

删除 `deepseek-harness-launcher.exe` 和 `%APPDATA%\io.github.hairyf.deepseek-harness-launcher\` 目录即可。
