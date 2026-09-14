# 浏览器访问 token（dsh 0.1.2+）—— launcher 0.3.7 修复

> 2026-09-14。用户报告：「启动 dsh web 以后，浏览器地址后面会有一个 token，
> 而我们系统里没有加这个 token 参数，所以打不开」。

## 现象

| 场景 | 结果 |
|---|---|
| launcher 点「打开 Harness 页面」/ 左键点托盘图标 | **401 打不开** |
| 手动在浏览器补上 `?token=...` | 正常打开 |

## 根因

dsh **0.1.2+** 给 Web UI 加了**进程启动令牌**校验（`dsh-client-connection` 的 `BrowserAuth`）：

```js
// dsh-client-connection/lib/index.js
const TOKEN_QUERY = "token";

authenticatedUrl(baseUrl) {
  const url = new URL(baseUrl);
  url.pathname = "/";
  url.search = "";
  url.hash = "";
  url.searchParams.set(TOKEN_QUERY, this.launchToken);  // ← 加 ?token=<随机值>
  return url.href;
}
```

- token 是**每次进程启动随机生成**的 base64url 字符串（43 字符，32 字节）
- 首次访问必须带 `?token=`；校验通过后 **303 跳转到干净的 `/`** 并种下签名 cookie
- 之后凭 cookie 访问；**不带 token 且无 cookie → 401**

dsh 自己启动时会打印带 token 的 URL（`dsh-web-app` 的 `announceReady`）：

```
dsh web: http://127.0.0.1:3197/?token=xJJIaaYvmVJDnOMAO5h8IZvvYfXa-xY0wI8cVignoQQ
```

**launcher 的问题**：各处都硬拼 `http://127.0.0.1:{port}`（**干净 URL**），
而这个 URL 在 0.1.2+ 上必然 401。

> 关键区分：dsh 源码注释明确写了
> 「The model and shell retain the **clean URL**」——
> 干净 URL 是给**模型/终端**看的，**浏览器必须用带 token 的**。
> launcher 之前误用了干净 URL 去开浏览器。

## 实测证据（本机，2026-09-14）

```
不带 token: HTTP 401   ← 同事看到的"打不开"
带 token:   HTTP 303 → 200（跟随重定向）
```

## 修复（0.3.7）

### 1. 抓取 token

启动后从 `dsh-launch-<pid>.log` 里提取（`workflow::extract_token_from_log`）：

- 按 `?token=` 直接扫，**不依赖 `dsh web:` 前缀**——该行是 console.log 输出，
  重定向后可能与其他输出交错
- token 字符集 = base64url（`A-Za-z0-9-_`），取到第一个非该字符集为止
- 带 LAN 地址时 dsh 会打印两个 token，取**第一个**（本机回环）
- 取不到（旧版 dsh 无该机制）→ `None`，URL 不带参数，**行为与旧版一致**

时机：dsh 在**端口 bind 之后**才打印该行，所以端口就绪后再读；
两者几乎同时，日志刷盘有毫秒级延迟 → 小步重试 5s（`read_launch_token`）。

### 2. 统一出口 `workflow::access_url(port)`

```rust
pub fn access_url(port: u16) -> String {
    // 有 token → http://127.0.0.1:{port}/?token=xxx
    // 无 token → http://127.0.0.1:{port}（旧版 dsh）
}
```

替换了所有**面向用户**的 URL 构造点：

| 位置 | 用途 |
|---|---|
| `tray.rs` 托盘左键 | 打开页面 |
| `tray.rs` `open-page` 菜单项 | 打开页面 |
| `tray.rs` 启动成功通知 | 显示可访问地址 |
| `tray.rs` profile 切换通知 | 显示可访问地址 |
| `main.rs` 自动启动通知 | 显示可访问地址 |
| `matrix_setup.rs` 向导进度 | 显示可访问地址 |
| `dsh_versions.rs` 版本切换日志 | 记录可访问地址 |

**保留干净 URL 的位置**（不该带 token）：`admin_bridge`（launcher 自己的
管理能力 API，与 dsh token 无关）。

### 3. 日志包脱敏（重要）

`dsh-launch-*.log` 现在含 token，而**日志包会发给管理员** →
token 是进程级访问凭据（拿到即可打开并操作同事的 Web GUI），必须掩码。

`logpack::redact` 新增 `redact_url_tokens`：处理嵌在 URL 中间的 `token=`
（无法用原来「按 `:` 切分整行」的规则覆盖），保留前 4 位便于比对。

```
dsh web: http://127.0.0.1:3197/?token=xJJI****
```

## 版本兼容

| dsh 版本 | 行为 |
|---|---|
| < 0.1.2（如 0.1.1-rc.2） | 打印干净 URL，无 token 机制 → launcher 用干净 URL（不变） |
| ≥ 0.1.2（如 0.1.2-rc.1） | 打印带 token URL → launcher 提取并带上 |

launcher 按「日志里有没有 `?token=`」自适应，**无需判断版本号**。

## 验证

```powershell
# 单测（含 8 项 token 相关）
cd src-tauri; cargo test --release

# 手工确认 dsh 打印的 URL
node <dsh>/lib/bin.js web --port 3197 --no-open
# → dsh web: http://127.0.0.1:3197/?token=...

# 确认不带 token 会 401
curl -o NUL -w "%{http_code}" http://127.0.0.1:3197/          # 401
curl -L -o NUL -w "%{http_code}" "http://127.0.0.1:3197/?token=<token>"  # 200
```
