# Harness Launcher × Caddy 集成测试

本目录（`../caddy/`）模拟企业内网部署：
Caddy 作为反向代理网关，把 `ai-conf.ict.cmcc` 转发到本机 launcher 中心服务端。

## 部署拓扑

```
客户端 launcher ──serverUrl: http://ai-conf.ict.cmcc──▶ Caddy(:80) ──reverse_proxy──▶ node server.js(:8081, --data launcher-data)
```

- hosts 已配置：`127.0.0.1 ai-roster.ict.cmcc ai-conf.ict.cmcc`
- Caddyfile：`ai-conf.ict.cmcc` → `127.0.0.1:8081`（launcher 服务端），
  `ai-roster.ict.cmcc` → `127.0.0.1:8765`（另一个服务，本测试不用）
- launcher 服务端数据目录：`launcher-data/`（config.json + clients/）

## 一键集成测试

在 PowerShell 中运行（需要已安装 Caddy，winget: `CaddyServer.Caddy`）：

```powershell
# 1. 启动 launcher 中心服务端（独立进程，数据指向 caddy/launcher-data）
$node = (Get-Command node).Source
Start-Process $node -ArgumentList @(
  "E:\ai-works\deepseek-harness-launcher\server\server.js",
  "--port", "8081",
  "--data", "E:\ai-works\caddy\launcher-data",
  "--token", "test123"
) -WindowStyle Hidden

# 2. 启动 Caddy（独立进程）
$caddy = "C:\Users\niukl\AppData\Local\Microsoft\WinGet\Packages\CaddyServer.Caddy_Microsoft.Winget.Source_8wekyb3d8bbwe\caddy.exe"
Start-Process $caddy -ArgumentList @("run", "--config", "E:\ai-works\caddy\Caddyfile", "--adapter", "caddyfile") -WindowStyle Hidden

# 3. 验证
#    经 Caddy 拉取配置（应返回 JSON，plugins 为空或含推荐）
Invoke-RestMethod "http://ai-conf.ict.cmcc/api/config"

# 4. 管理员设置推荐插件（经 Caddy，需 token）
Invoke-RestMethod "http://ai-conf.ict.cmcc/api/config" -Method Post `
  -Headers @{ "X-Admin-Token" = "test123" } `
  -Body '{"plugins":["dsh-nested-followups","@noob-stupid/dsh-plugin-console"]}' `
  -ContentType "application/json"

# 5. 客户端 launcher 同步（launcher-config.json 配 serverUrl = http://ai-conf.ict.cmcc）
#    启动 launcher，等待一个轮询周期，然后查看服务端收到的上报：
Invoke-RestMethod "http://ai-conf.ict.cmcc/api/status" -Headers @{ "X-Admin-Token" = "test123" }
```

## 验证点（已实测通过）

| 验证项 | 结果 |
|---|---|
| Caddyfile 语法校验 `caddy validate` | Valid configuration |
| `ai-conf.ict.cmcc/api/config` 经 Caddy → 8081 | HTTP 200，返回 config JSON |
| `ai-conf.ict.cmcc/api/config` POST 设推荐插件 | 保存成功（plugins 更新） |
| `ai-conf.ict.cmcc/admin` 管理页 | HTTP 200，含「推荐插件」界面 |
| `ai-roster.ict.cmcc` 路由隔离 | 走 8765（502 证明未错路由到 launcher） |
| launcher 客户端经 Caddy 域名同步 | 日志「同步成功」，服务端 /api/status 看到新客户端（clientId/pending/hostname） |

## 清理

```powershell
Get-Process node, caddy -ErrorAction SilentlyContinue | Stop-Process -Force
# 可选：重置测试数据
Remove-Item "E:\ai-works\caddy\launcher-data\clients\*" -Force
```

## 备注

- 管理 token：启动服务端时 `--token test123`（测试用）；生产请换强口令。
- 服务端默认 `--data` 是自己的 `server/data`；接入 Caddy 场景应显式指向 `launcher-data`。
- 若 80 端口被占用：`netstat -ano | findstr :80` 查占用进程；Caddy 需以管理员权限监听 80
  （本机测试时 PowerShell 会话已有权限则无需）。
