# Harness Launcher 中心服务端

企业管理员部署在内网服务器上的管理控制台：向各同事办公电脑上的
DeepSeek Harness Launcher 下发「插件策略 + 托盘菜单策略」，并收集每台客户端的
插件详情、菜单、配置状态，帮助管理员引导小白用户（管理员配好，客户端自动执行）。

## 快速开始

```bash
cd server
node server.js --port 8080 --token 你的管理口令
```

- **跨平台**：纯 Node.js（`node:http`/`node:fs`/`node:path`），Linux / Windows / macOS 均可运行，
  服务端包不区分平台（一个 zip 通用）。
- 无任何 npm 依赖，只要 Node.js ≥ 18。
- 默认数据目录 `./data`（可用 `--data <dir>` 指定）；`config.json` 与 `clients/` 自动创建。
- 建议部署在 Linux 服务器：`nohup node server.js --port 8080 --token xxx &` 或配 systemd 常驻。

## 端点

| 端点 | 方法 | 用途 | 鉴权 |
|---|---|---|---|
| `/api/config` | GET | 客户端拉取插件策略 + 菜单策略 | 无 |
| `/api/config` | POST | 管理员保存策略（plugins + managedMenu） | `X-Admin-Token` |
| `/api/plugins/meta` | GET | 查询插件元信息（描述/最新版/主页，registry 拉取带缓存） | 无 |
| `/api/sync` | POST | 客户端上报插件详情/菜单/配置状态 | 无 |
| `/api/status` | GET | 查看所有客户端完整状态 | `X-Admin-Token` |
| `/admin` | GET | 管理控制台（策略编辑 + 客户端状态 + 健康概览） | `X-Admin-Token` |

浏览器打开 `http://<服务器IP>:8080/admin` 即可管理（首次会提示输入 token，可存于 localStorage）。

插件元信息：`/api/plugins/meta?names=a,b,c` 从 npm registry（npmjs 优先 → npmmirror → 内网
`REGISTRY_OVERRIDE` 环境变量指定）拉取每个插件的描述、最新版本、主页，10 分钟缓存；
管理页「插件策略」据此展示详情卡片，方便管理员判断每个插件用途。

### 服务端不直连外网时的中转（推荐）

服务端若**不能访问外网**（内网隔离），管理页查包信息改由**管理员本机 launcher 中转**：

1. 管理员在 launcher 托盘「管理能力」开启外网代理 → 本机 `127.0.0.1:3410` 起本地 API
2. 管理页自动探测（客户端上报的 `bridgeStatus`）或手动输入端口连接
3. 管理页 `fetch http://127.0.0.1:3410/api/registry/meta?name=...` → launcher 查外网 registry 返回

这样服务端全程不出网，只靠管理员电脑中转。本地 API 仅绑 127.0.0.1，可选 token 防护。

## 策略字段（`data/config.json`）

```json
{
  "version": 2,
  "plugins": ["dsh-nested-followups", "@noob-stupid/dsh-plugin-console"],
  "managedMenu": {
    "enabled": true,
    "quickLinks": [
      { "label": "公司OA", "url": "http://oa.internal" },
      { "label": "Wiki", "url": "https://wiki.internal" }
    ]
  },
  "updatedAt": "2026-08-28T00:00:00.000Z",
  "baseUrl": ""
}
```

- `plugins`：**应装插件清单**（不带版本号，客户端装最新版）。客户端任一 profile 装了即满足；
  未装的客户端弹通知 + 托盘「安装」菜单，点击自动安装。
- `managedMenu`：**托盘菜单策略**。`enabled=true` 时客户端托盘「常用网址」菜单**完全展示策略项**
  （用户本地菜单不被覆盖，仅展示层切换）；`enabled=false` 回退用户自己的菜单。
- 菜单项校验：label 非空、url 仅 `http://` / `https://`（拒绝 `javascript:`/`file:` 等注入面）。

## 客户端上报（`data/clients/<clientId>.json`）

每台客户端每次成功上报覆盖写入：

```json
{
  "clientId": "uuid",
  "hostname": "PC-NAME",
  "dshVersion": "0.1.1-rc.2",
  "launcherVersion": "0.1.0",
  "installed": ["dsh-nested-followups"],
  "pending": ["@noob-stupid/dsh-plugin-console"],
  "plugins": [
    { "name": "dsh-nested-followups", "version": "0.2.2", "description": "...", "profile": "web", "client": true }
  ],
  "menu": [{ "label": "公司OA", "url": "http://oa.internal" }],
  "menuApplied": true,
  "profiles": ["web", "matrix"],
  "configState": { "profile": "web", "port": 3180 },
  "offline": false,
  "lastSyncAt": "2026-08-28T10:00:00.000Z"
}
```

- `plugins`：跨所有 profile 的已装插件详情（name/version/description/profile/是否贡献 web UI）。
- `menu`：客户端实际展示的托盘菜单；`menuApplied`：菜单策略是否已应用。
- 管理页据此显示每台客户端的健康状态（正常 / 需关注：缺插件、菜单未应用、离线）与概览统计。

## 安全提示

- 面向**内网**部署：拉取 `/api/config` 免鉴权；管理写操作靠 `--token`（请求头 `X-Admin-Token`）。
- 建议服务器防火墙仅放行内网；如需公网请置于反向代理后并启用 TLS。
- 客户端 ID 是随机 UUID，仅用于区分机器，不含主机名等敏感信息（hostname 可选上报）。
- `baseUrl`：预留（如未来指向内网 npm 私服）。

## 客户端状态（`data/clients/<clientId>.json`）

每台客户端每次成功上报覆盖写入：

```json
{
  "clientId": "uuid",
  "hostname": "PC-NAME",
  "dshVersion": "0.1.1-rc.2",
  "launcherVersion": "0.1.0",
  "installed": ["dsh-nested-followups"],
  "pending": ["@noob-stupid/dsh-plugin-console"],
  "offline": false,
  "lastSyncAt": "2026-08-28T10:00:00.000Z"
}
```

## 安全提示

- 面向**内网**部署：拉取 `/api/config` 免鉴权；管理写操作靠 `--token`（请求头 `X-Admin-Token`）。
- 建议服务器防火墙仅放行内网；如需公网请置于反向代理后并启用 TLS。
- 客户端 ID 是随机 UUID，仅用于区分机器，不含主机名等敏感信息（hostname 可选上报）。
