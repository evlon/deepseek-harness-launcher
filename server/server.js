#!/usr/bin/env node
/**
 * 企业中心服务端 —— DeepSeek Harness Launcher 的配置同步服务。
 *
 * 零依赖（node:http + node:fs），管理员在内网服务器上运行：
 *   node server.js [--port 8080] [--data ./data] [--token <admin-token>]
 *
 * 端点：
 *   GET  /api/config          客户端拉取推荐插件与配置（免鉴权）
 *   POST /api/sync            客户端上报同步状态（免鉴权，见 adminToken 说明）
 *   GET  /api/status          管理员查看所有客户端同步情况（需 token）
 *   GET  /admin               单文件管理页（需 token）
 *   POST /api/config          更新推荐插件清单（需 token）
 *
 * 数据（JSON 文件，--data 目录，默认 ./data）：
 *   config.json               { version, plugins: [...], updatedAt, baseUrl }
 *   clients/<clientId>.json   每台客户端的最近一次上报
 */
"use strict";

const http = require("node:http");
const fs = require("node:fs");
const path = require("node:path");

// ---------- 参数 ----------
function parseArgs(argv) {
  const args = { port: 8080, data: path.join(__dirname, "data"), token: "" };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--port") args.port = parseInt(argv[++i], 10) || 8080;
    else if (a === "--data") args.data = path.resolve(argv[++i]);
    else if (a === "--token") args.token = argv[++i] || "";
  }
  return args;
}
const ARGS = parseArgs(process.argv.slice(2));

// ---------- 数据目录 ----------
const CONFIG_PATH = path.join(ARGS.data, "config.json");
const CLIENTS_DIR = path.join(ARGS.data, "clients");

function ensureData() {
  fs.mkdirSync(ARGS.data, { recursive: true });
  fs.mkdirSync(CLIENTS_DIR, { recursive: true });
  if (!fs.existsSync(CONFIG_PATH)) {
    writeConfig(defaultConfig());
  }
}

function defaultConfig() {
  return {
    version: 2,
    plugins: [],
    managedMenu: { enabled: false, quickLinks: [] },
    clientDefaults: {},
    mirrorSettings: { registry: "https://registry.ict.cmcc", tokenValue: "" },
    updatedAt: new Date().toISOString(),
    baseUrl: "",
  };
}

function readConfig() {
  try {
    return JSON.parse(fs.readFileSync(CONFIG_PATH, "utf8"));
  } catch {
    return defaultConfig();
  }
}

/** 归一化旧版 config：补 managedMenu / clientDefaults / mirrorSettings 默认值，保证旧数据平滑升级。 */
function normalizeConfig(cfg) {
  if (cfg && typeof cfg === "object") {
    if (typeof cfg.managedMenu !== "object" || cfg.managedMenu === null) {
      cfg.managedMenu = { enabled: false, quickLinks: [] };
    }
    if (typeof cfg.clientDefaults !== "object" || cfg.clientDefaults === null) {
      cfg.clientDefaults = {};
    }
    if (typeof cfg.mirrorSettings !== "object" || cfg.mirrorSettings === null) {
      cfg.mirrorSettings = { registry: "https://registry.ict.cmcc", tokenValue: "" };
    }
    if (typeof cfg.plugins !== "object" || !Array.isArray(cfg.plugins)) cfg.plugins = [];
  }
  return cfg;
}

function writeConfig(cfg) {
  cfg.updatedAt = new Date().toISOString();
  fs.writeFileSync(CONFIG_PATH, JSON.stringify(normalizeConfig(cfg), null, 2), "utf8");
}

function listClients() {
  try {
    return fs.readdirSync(CLIENTS_DIR).filter((f) => f.endsWith(".json")).map((f) => {
      try {
        return JSON.parse(fs.readFileSync(path.join(CLIENTS_DIR, f), "utf8"));
      } catch {
        return null;
      }
    }).filter(Boolean);
  } catch {
    return [];
  }
}

/** 归一化旧版客户端记录：补新字段默认值，保证 /api/status 输出结构稳定。 */
function normalizeClientRecord(c) {
  if (!c || typeof c !== "object") return null;
  return {
    clientId: c.clientId || "",
    hostname: c.hostname || "",
    dshVersion: c.dshVersion || "",
    launcherVersion: c.launcherVersion || "",
    installed: Array.isArray(c.installed) ? c.installed : [],
    pending: Array.isArray(c.pending) ? c.pending : [],
    plugins: Array.isArray(c.plugins) ? c.plugins : [],
    menu: Array.isArray(c.menu) ? c.menu : [],
    menuApplied: !!c.menuApplied,
    profiles: Array.isArray(c.profiles) ? c.profiles : [],
    configState: c.configState && typeof c.configState === "object"
      ? { profile: c.configState.profile || "", port: Number(c.configState.port) || 0 }
      : { profile: "", port: 0 },
    bridgeStatus: c.bridgeStatus && typeof c.bridgeStatus === "object"
      ? { enabled: !!c.bridgeStatus.enabled, port: Number(c.bridgeStatus.port) || 0 }
      : { enabled: false, port: 0 },
    offline: !!c.offline,
    lastSyncAt: c.lastSyncAt || "",
  };
}

// ---------- 工具 ----------
function send(res, code, obj) {
  const body = JSON.stringify(obj);
  res.writeHead(code, {
    "Content-Type": "application/json; charset=utf-8",
    "Cache-Control": "no-store",
    "Content-Length": Buffer.byteLength(body),
  });
  res.end(body);
}

function readBody(req, limit = 1 << 20) {
  return new Promise((resolve, reject) => {
    let size = 0;
    const chunks = [];
    req.on("data", (c) => {
      size += c.length;
      if (size > limit) {
        reject(new Error("body too large"));
        req.destroy();
        return;
      }
      chunks.push(c);
    });
    req.on("end", () => {
      try {
        resolve(chunks.length ? JSON.parse(Buffer.concat(chunks).toString("utf8")) : {});
      } catch (e) {
        reject(new Error("invalid json: " + e.message));
      }
    });
    req.on("error", reject);
  });
}

/** npm 包名合法性：作用域包或普通包，字母数字 . - _ ~；禁止空格/斜杠/引号（防注入）。 */
function validPackageName(name) {
  return (
    typeof name === "string" &&
    /^(?:@[a-z0-9-~][a-z0-9-._~]*\/)?[a-z0-9-~][a-z0-9-._~]*$/.test(name) &&
    !name.includes("..")
  );
}

/** 菜单项合法性：label 非空、url 仅 http/https（拒绝 file:/javascript: 等注入面）。 */
function validQuickLink(item) {
  return (
    item &&
    typeof item === "object" &&
    typeof item.label === "string" &&
    item.label.trim().length > 0 &&
    item.label.length <= 64 &&
    typeof item.url === "string" &&
    /^https?:\/\/\S+$/i.test(item.url.trim()) &&
    item.url.length <= 2048
  );
}

/** 校验 managedMenu 策略：返回 { ok, error }。 */
function validateManagedMenu(m) {
  if (m === undefined || m === null) return { ok: true };
  if (typeof m !== "object" || Array.isArray(m)) return { ok: false, error: "managedMenu 必须是对象" };
  if (typeof m.enabled !== "boolean") return { ok: false, error: "managedMenu.enabled 必须是布尔值" };
  if (!Array.isArray(m.quickLinks)) return { ok: false, error: "managedMenu.quickLinks 必须是数组" };
  const bad = m.quickLinks.filter((q) => !validQuickLink(q));
  if (bad.length) return { ok: false, error: "managedMenu.quickLinks 含非法项（label 非空、url 需 http/https）" };
  return { ok: true };
}

function authorized(req) {
  if (!ARGS.token) return true; // 未设 token 时管理操作仅限内网（不鉴权）
  const h = req.headers["x-admin-token"];
  return typeof h === "string" && h === ARGS.token;
}

// ---------- 插件元信息（registry 查询 + 缓存） ----------

const META_CACHE = new Map(); // name -> { meta, fetchedAt }
const META_CACHE_TTL_MS = 10 * 60 * 1000; // 10 分钟缓存
const REGISTRIES = [
  { url: "https://registry.npmjs.org", label: "npmjs" },
  { url: "https://registry.npmmirror.com", label: "npmmirror" },
  // 内网 registry（如企业私服）：命中即优先，放在最后作兜底
];
// 允许通过环境变量注入内网 registry（server 启动时 REGISTRY_OVERRIDE=https://registry.ict.cmcc）
if (process.env.REGISTRY_OVERRIDE) {
  REGISTRIES.unshift({ url: process.env.REGISTRY_OVERRIDE.replace(/\/+$/, ""), label: "内网" });
}

/** 从 registry 拉取单个插件元信息（npmjs → npmmirror → 内网 依次尝试）。 */
async function fetchPluginMeta(name) {
  for (const reg of REGISTRIES) {
    try {
      const ctrl = new AbortController();
      const timer = setTimeout(() => ctrl.abort(), 6000);
      const res = await fetch(`${reg.url}/${encodeURIComponent(name).replace(/%2F/gi, "/")}`, {
        signal: ctrl.signal,
        headers: { "User-Agent": "dsh-harness-launcher-admin/0.1" },
      });
      clearTimeout(timer);
      if (!res.ok) continue;
      const data = await res.json();
      const latest = (data["dist-tags"] && data["dist-tags"].latest) || "";
      const ver = latest ? data.versions && data.versions[latest] : null;
      return {
        name,
        latest,
        description: (ver && ver.description) || data.description || "",
        homepage: (ver && ver.homepage) || data.homepage || "",
        repository: (ver && ver.repository && ver.repository.url) || "",
        registry: reg.label,
      };
    } catch (e) {
      // 尝试下一个源
    }
  }
  return { name, latest: "", description: "", homepage: "", repository: "", registry: "" };
}

/** 批量查询插件元信息（带缓存），失败返回 null 字段不阻断。 */
async function fetchPluginsMeta(names) {
  const out = [];
  for (const name of names) {
    const cached = META_CACHE.get(name);
    if (cached && Date.now() - cached.fetchedAt < META_CACHE_TTL_MS) {
      out.push(cached.meta);
      continue;
    }
    const meta = await fetchPluginMeta(name);
    META_CACHE.set(name, { meta, fetchedAt: Date.now() });
    out.push(meta);
  }
  return out;
}

// ---------- 路由 ----------
async function route(req, res) {
  const url = new URL(req.url, "http://localhost");
  const p = url.pathname;

  // CORS（管理页与客户端可能跨源）
  res.setHeader("Access-Control-Allow-Origin", "*");
  res.setHeader("Access-Control-Allow-Methods", "GET, POST, OPTIONS");
  res.setHeader("Access-Control-Allow-Headers", "Content-Type, X-Admin-Token");
  if (req.method === "OPTIONS") {
    res.writeHead(204);
    res.end();
    return;
  }

  // 客户端拉取配置（免鉴权，内网）
  if (p === "/api/config" && req.method === "GET") {
    return send(res, 200, normalizeConfig(readConfig()));
  }

  // 管理员更新配置
  if (p === "/api/config" && req.method === "POST") {
    if (!authorized(req)) return send(res, 403, { error: "unauthorized" });
    let body;
    try {
      body = await readBody(req);
    } catch (e) {
      return send(res, 400, { error: e.message });
    }
    const plugins = Array.isArray(body.plugins) ? body.plugins : [];
    const bad = plugins.filter((x) => !validPackageName(x));
    if (bad.length) return send(res, 400, { error: "invalid package names: " + bad.join(", ") });
    const menuCheck = validateManagedMenu(body.managedMenu);
    if (!menuCheck.ok) return send(res, 400, { error: menuCheck.error });
    const cfg = normalizeConfig(readConfig());
    cfg.plugins = [...new Set(plugins)]; // 去重保序
    if (body.managedMenu !== undefined) {
      cfg.managedMenu = {
        enabled: !!body.managedMenu.enabled,
        quickLinks: (body.managedMenu.quickLinks || []).map((q) => ({
          label: q.label.trim(),
          url: q.url.trim(),
        })),
      };
    }
    // 客户端默认配置覆盖（clientDefaults）：校验合法字段
    if (body.clientDefaults !== undefined) {
      const cd = body.clientDefaults;
      if (typeof cd !== "object" || cd === null || Array.isArray(cd)) {
        return send(res, 400, { error: "clientDefaults 必须是对象" });
      }
      const cleaned = {};
      // npmRegistry / ghMirrorPrefix 支持字符串或字符串数组（多源）
      const normList = (v) => {
        if (typeof v === "string") return v.trim() ? [v.trim()] : null;
        if (Array.isArray(v)) {
          const out = v.filter((x) => typeof x === "string" && x.trim()).map((x) => x.trim());
          return out.length ? out : null;
        }
        return undefined; // 非法类型
      };
      if (cd.npmRegistry !== undefined) {
        const n = normList(cd.npmRegistry);
        if (n === undefined) return send(res, 400, { error: "clientDefaults.npmRegistry 必须是字符串或字符串数组" });
        if (n !== null) cleaned.npmRegistry = n;
      }
      if (cd.ghMirrorPrefix !== undefined) {
        const g = normList(cd.ghMirrorPrefix);
        if (g === undefined) return send(res, 400, { error: "clientDefaults.ghMirrorPrefix 必须是字符串或字符串数组" });
        if (g !== null) cleaned.ghMirrorPrefix = g;
      }
      if (cd.port !== undefined) {
        const p = Number(cd.port);
        if (!Number.isInteger(p) || p < 1 || p > 65535) return send(res, 400, { error: "clientDefaults.port 必须在 1-65535" });
        cleaned.port = p;
      }
      if (cd.syncIntervalSecs !== undefined) {
        const s = Number(cd.syncIntervalSecs);
        if (!Number.isInteger(s) || s < 30) return send(res, 400, { error: "clientDefaults.syncIntervalSecs 必须 >= 30" });
        cleaned.syncIntervalSecs = s;
      }
      if (cd.profile !== undefined) {
        if (typeof cd.profile !== "string" || !/^[a-zA-Z0-9_-]+$/.test(cd.profile)) return send(res, 400, { error: "clientDefaults.profile 非法" });
        cleaned.profile = cd.profile;
      }
      if (cd.useSystemNode !== undefined) {
        if (typeof cd.useSystemNode !== "boolean") return send(res, 400, { error: "clientDefaults.useSystemNode 必须是布尔值" });
        cleaned.useSystemNode = cd.useSystemNode;
      }
      cfg.clientDefaults = cleaned;
    }
    // 镜像上传设置（mirrorSettings）：registry 合法 URL + tokenValue（发布凭证，存服务端）
    if (body.mirrorSettings !== undefined) {
      const ms = body.mirrorSettings;
      if (typeof ms !== "object" || ms === null || Array.isArray(ms)) {
        return send(res, 400, { error: "mirrorSettings 必须是对象" });
      }
      const cleanedMs = {};
      if (ms.registry !== undefined) {
        if (typeof ms.registry !== "string" || !/^https?:\/\/\S+$/.test(ms.registry.trim())) {
          return send(res, 400, { error: "mirrorSettings.registry 必须是 http(s) 地址" });
        }
        cleanedMs.registry = ms.registry.trim();
      }
      if (ms.tokenValue !== undefined) {
        if (typeof ms.tokenValue !== "string") return send(res, 400, { error: "mirrorSettings.tokenValue 必须是字符串" });
        cleanedMs.tokenValue = ms.tokenValue.trim();
      }
      cfg.mirrorSettings = Object.assign(
        { registry: "https://registry.ict.cmcc", tokenValue: "" },
        cleanedMs
      );
    }
    if (typeof body.baseUrl === "string") cfg.baseUrl = body.baseUrl;
    if (typeof body.version === "number") cfg.version = body.version;
    writeConfig(cfg);
    return send(res, 200, normalizeConfig(cfg));
  }

  // 客户端上报同步状态
  if (p === "/api/sync" && req.method === "POST") {
    let body;
    try {
      body = await readBody(req);
    } catch (e) {
      return send(res, 400, { error: e.message });
    }
    const clientId = String(body.clientId || "").trim();
    if (!clientId || !/^[a-zA-Z0-9-]{8,64}$/.test(clientId)) {
      return send(res, 400, { error: "invalid clientId" });
    }
    const record = {
      clientId,
      hostname: String(body.hostname || "").slice(0, 128),
      dshVersion: String(body.dshVersion || "").slice(0, 64),
      launcherVersion: String(body.launcherVersion || "").slice(0, 64),
      installed: Array.isArray(body.installed) ? body.installed.filter((x) => typeof x === "string") : [],
      pending: Array.isArray(body.pending) ? body.pending.filter((x) => typeof x === "string") : [],
      // 插件详情（跨所有 profile）：[{name, version, description, profile, client}]
      plugins: Array.isArray(body.plugins)
        ? body.plugins
            .filter((p) => p && typeof p.name === "string")
            .map((p) => ({
              name: String(p.name).slice(0, 128),
              version: String(p.version || "").slice(0, 64),
              description: String(p.description || "").slice(0, 256),
              profile: String(p.profile || "").slice(0, 64),
              client: !!p.client,
            }))
        : [],
      // 实际托盘菜单
      menu: Array.isArray(body.menu)
        ? body.menu.filter((m) => m && typeof m.label === "string").map((m) => ({
            label: String(m.label).slice(0, 64),
            url: String(m.url || "").slice(0, 2048),
          }))
        : [],
      // 菜单策略是否已应用
      menuApplied: !!body.menuApplied,
      // 客户端 profile 列表
      profiles: Array.isArray(body.profiles) ? body.profiles.filter((x) => typeof x === "string").map((x) => String(x).slice(0, 64)) : [],
      // 配置状态
      configState: {
        profile: String((body.configState && body.configState.profile) || "").slice(0, 64),
        port: Number((body.configState && body.configState.port) || 0) || 0,
      },
      // 管理能力（外网代理网关）状态
      bridgeStatus: {
        enabled: !!(body.bridgeStatus && body.bridgeStatus.enabled),
        port: Number((body.bridgeStatus && body.bridgeStatus.port) || 0) || 0,
      },
      offline: !!body.offline,
      lastSyncAt: new Date().toISOString(),
    };
    fs.writeFileSync(path.join(CLIENTS_DIR, clientId + ".json"), JSON.stringify(record, null, 2), "utf8");
    return send(res, 200, { ok: true, lastSyncAt: record.lastSyncAt });
  }

  // 管理员查看所有客户端状态
  if (p === "/api/status" && req.method === "GET") {
    if (!authorized(req)) return send(res, 403, { error: "unauthorized" });
    const clients = listClients()
      .map(normalizeClientRecord)
      .sort((a, b) => (b.lastSyncAt || "").localeCompare(a.lastSyncAt || ""));
    return send(res, 200, { clients });
  }

  // 插件元信息查询（管理页展示应装清单详情用；名称来自 /api/config 的 plugins，免鉴权）
  if (p === "/api/plugins/meta" && req.method === "GET") {
    const namesParam = url.searchParams.get("names") || "";
    const names = namesParam.split(",").map((s) => s.trim()).filter((s) => s && validPackageName(s));
    if (!names.length) return send(res, 200, { plugins: [] });
    const plugins = await fetchPluginsMeta(names);
    return send(res, 200, { plugins });
  }

  // 单文件管理页：页面本身可访问（内网），鉴权在页面内的 token 输入 + 各 API 调用。
  // 服务端未设 --token 时，页面直接可用；设了 token，页面打开后弹出输入框。
  if (p === "/admin" && req.method === "GET") {
    res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
    res.end(adminPageHtml());
    return;
  }

  // 管理页 JS（独立文件，避免模板字符串转义问题）
  if (p === "/admin.js" && req.method === "GET") {
    const jsPath = path.join(__dirname, "admin.js");
    try {
      const js = fs.readFileSync(jsPath, "utf8");
      res.writeHead(200, { "Content-Type": "application/javascript; charset=utf-8", "Cache-Control": "no-store" });
      res.end(js);
    } catch (e) {
      return send(res, 500, { error: "admin.js 读取失败: " + e.message });
    }
    return;
  }

  send(res, 404, { error: "not found" });
}

// ---------- 管理页 ----------
function adminPageHtml() {
  return `<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Harness 中心管理</title>
<style>
  :root{
    --bg:#f4f6fb; --card:#fff; --line:#e5e8f0; --line2:#eef0f6;
    --text:#1a2233; --muted:#7a8394; --faint:#a6aebe;
    --primary:#3b66f0; --primary-weak:#eaf0ff; --primary-ink:#3b66f0;
    --green:#16a34a; --green-bg:#e8f7ee; --amber:#d97706; --amber-bg:#fef3e2;
    --red:#dc2626; --red-bg:#fdeaea; --red-weak:#fff1f1;
    --shadow:0 1px 2px rgba(20,30,60,.04),0 8px 24px rgba(20,30,60,.06);
    --radius:12px;
  }
  *{box-sizing:border-box}
  html,body{margin:0;padding:0}
  body{font-family:-apple-system,"Segoe UI",Roboto,"PingFang SC","Microsoft YaHei",sans-serif;
    background:var(--bg);color:var(--text);font-size:14px;line-height:1.55;
    -webkit-font-smoothing:antialiased}
  a{color:var(--primary);text-decoration:none}

  /* ── 顶栏 ── */
  .topbar{position:sticky;top:0;z-index:20;display:flex;align-items:center;gap:16px;
    height:60px;padding:0 24px;background:#fff;border-bottom:1px solid var(--line)}
  .brand{display:flex;align-items:center;gap:10px;font-weight:650;font-size:15px;white-space:nowrap}
  .brand .logo{width:30px;height:30px;border-radius:8px;background:linear-gradient(135deg,#3b66f0,#7a5cf0);
    display:flex;align-items:center;justify-content:center;color:#fff;font-size:15px;font-weight:700}
  .topbar .spacer{flex:1}
  .sync-hint{font-size:12px;color:var(--muted)}
  .sync-hint b{color:var(--text)}

  /* ── Tab 导航 ── */
  .tabs{position:sticky;top:60px;z-index:19;display:flex;gap:4px;padding:10px 24px 0;
    background:var(--bg);border-bottom:1px solid var(--line)}
  .tab{appearance:none;border:0;background:transparent;cursor:pointer;font-size:14px;
    padding:9px 16px;color:var(--muted);border-radius:9px 9px 0 0;font-weight:500;
    border-bottom:2px solid transparent;transition:.15s}
  .tab:hover{color:var(--text)}
  .tab.active{color:var(--primary);font-weight:650;border-bottom-color:var(--primary);background:#fff}

  /* ── 主区 ── */
  main{max-width:1180px;margin:0 auto;padding:24px}
  .view{display:none}
  .view.active{display:block;animation:fade .18s ease}
  @keyframes fade{from{opacity:0;transform:translateY(4px)}to{opacity:1;transform:none}}

  .card{background:var(--card);border:1px solid var(--line);border-radius:var(--radius);
    box-shadow:var(--shadow);padding:20px;margin-bottom:18px}
  .card-head{display:flex;align-items:flex-start;justify-content:space-between;gap:12px;margin-bottom:16px}
  .card-title{font-size:15px;font-weight:650;margin:0}
  .card-desc{color:var(--muted);font-size:12.5px;margin-top:3px}
  .empty{color:var(--faint);font-size:13px;padding:14px 2px}

  /* ── 按钮 ── */
  .btn{appearance:none;display:inline-flex;align-items:center;gap:6px;border:1px solid var(--line);
    background:#fff;color:var(--text);padding:8px 14px;border-radius:8px;cursor:pointer;
    font-size:13.5px;font-weight:550;transition:.15s}
  .btn:hover{border-color:#cdd4e4;background:#fafbfe}
  .btn.primary{background:var(--primary);border-color:var(--primary);color:#fff}
  .btn.primary:hover{background:#2f55d8;border-color:#2f55d8}
  .btn.ghost{background:transparent;border-color:transparent;color:var(--muted)}
  .btn.ghost:hover{color:var(--text);background:#f2f4f9}
  .btn.sm{padding:5px 10px;font-size:12.5px;border-radius:7px}
  .btn.danger{color:var(--red);border-color:transparent;background:transparent}
  .btn.danger:hover{background:var(--red-bg)}

  /* ── 输入 ── */
  .input{width:100%;padding:8px 12px;border:1px solid var(--line);border-radius:8px;
    font-size:13.5px;font-family:inherit;color:var(--text);background:#fff;transition:.15s}
  .input:focus{outline:none;border-color:var(--primary);box-shadow:0 0 0 3px var(--primary-weak)}
  .field{margin-bottom:12px}
  .field label{display:block;font-size:12.5px;color:var(--muted);margin-bottom:5px;font-weight:550}
  .row{display:flex;gap:10px;align-items:center}
  .row .input{flex:1}

  /* ── KPI 卡片 ── */
  .kpis{display:grid;grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:14px;margin-bottom:20px}
  .kpi{background:#fff;border:1px solid var(--line);border-radius:var(--radius);box-shadow:var(--shadow);
    padding:16px 18px;display:flex;align-items:center;gap:14px}
  .kpi .ico{width:42px;height:42px;border-radius:10px;display:flex;align-items:center;justify-content:center;font-size:19px}
  .kpi .num{font-size:24px;font-weight:700;line-height:1.1}
  .kpi .lbl{font-size:12px;color:var(--muted)}
  .kpi.g .ico{background:var(--green-bg)} .kpi.g .num{color:var(--green)}
  .kpi.b .ico{background:var(--primary-weak)} .kpi.b .num{color:var(--primary)}
  .kpi.r .ico{background:var(--red-bg)} .kpi.r .num{color:var(--red)}
  .kpi.a .ico{background:var(--amber-bg)} .kpi.a .num{color:var(--amber)}

  /* ── chips ── */
  .chips{display:flex;flex-wrap:wrap;gap:6px}
  .chip{display:inline-flex;align-items:center;gap:5px;background:var(--primary-weak);color:var(--primary-ink);
    border-radius:20px;padding:3px 10px;font-size:12px;font-weight:550;max-width:100%}
  .chip .x{cursor:pointer;opacity:.55;font-weight:700;margin-left:2px}
  .chip .x:hover{opacity:1}
  .chip.dim{background:#f0f2f7;color:var(--muted)}
  .chip.pending{background:var(--red-weak);color:var(--red)}
  .chip .ver{opacity:.6;font-weight:400}
  .chip .pf{background:rgba(0,0,0,.06);border-radius:10px;padding:0 6px;font-size:10.5px;opacity:.75}
  .chip .web{opacity:.55;font-size:10.5px}

  /* ── 应装插件详情卡片 ── */
  .plugin-cards{display:grid;grid-template-columns:repeat(auto-fill,minmax(300px,1fr));gap:12px;margin-top:4px}
  .pcard{background:#fff;border:1px solid var(--line);border-radius:11px;box-shadow:var(--shadow);padding:14px 16px;
    display:flex;flex-direction:column;gap:8px;transition:.15s}
  .pcard:hover{border-color:#c9d3ee;box-shadow:0 4px 16px rgba(59,102,240,.1)}
  .pcard .phead{display:flex;align-items:center;gap:8px;justify-content:space-between}
  .pcard .pname{font-weight:650;font-size:14px;font-family:ui-monospace,SFMono-Regular,Consolas,monospace;word-break:break-all}
  .pcard .pver{font-size:11.5px;color:var(--muted);background:#f2f4f9;border-radius:10px;padding:1px 8px;white-space:nowrap}
  .pcard .pdesc{font-size:12.5px;color:var(--muted);line-height:1.5;min-height:20px}
  .pcard .pmeta{display:flex;align-items:center;gap:8px;font-size:11px;color:var(--faint);flex-wrap:wrap}
  .pcard .pmeta a{color:var(--primary);word-break:break-all}
  .pcard .pfoot{display:flex;justify-content:flex-end}
  .pcard.loading{opacity:.6}
  .pcard .src{background:#eef6ee;color:#1a7f37;border-radius:10px;padding:0 6px;font-size:10.5px}
  .pcard .missing{color:var(--amber);font-size:12px}
  /* 同步状态徽章 */
  .sync-badge{display:inline-flex;align-items:center;gap:4px;border-radius:12px;padding:2px 10px;font-size:11.5px;font-weight:600;white-space:nowrap}
  .sync-badge.synced{background:var(--green-bg);color:var(--green)}
  .sync-badge.unsynced{background:var(--amber-bg);color:var(--amber)}
  .sync-badge.checking{background:#eef0f6;color:var(--muted)}
  .pcard .sync-btn{margin-left:auto}
  .sync-toolbar{display:flex;align-items:center;gap:8px;flex-wrap:wrap}

  /* ── 状态点 ── */
  .dot{width:8px;height:8px;border-radius:50%;display:inline-block;margin-right:6px}
  .dot.ok{background:var(--green)} .dot.bad{background:var(--red)} .dot.warn{background:var(--amber)} .dot.gray{background:#c4cbd8}
  .status{font-size:12.5px;font-weight:600;display:inline-flex;align-items:center}
  .status.ok{color:var(--green)} .status.bad{color:var(--red)} .status.warn{color:var(--amber)} .status.gray{color:var(--muted)}

  /* ── 菜单策略行 ── */
  .menu-item{display:flex;gap:8px;align-items:center;padding:8px;border:1px solid var(--line2);
    border-radius:9px;margin-bottom:8px;background:#fbfcfe}
  .menu-item .drag{color:var(--faint);cursor:grab;user-select:none;padding:0 4px;font-size:15px}
  .menu-item .idx{width:22px;height:22px;border-radius:6px;background:var(--primary-weak);color:var(--primary);
    display:flex;align-items:center;justify-content:center;font-size:11.5px;font-weight:700;flex-shrink:0}
  .switch{position:relative;width:40px;height:22px;flex-shrink:0}
  .switch input{opacity:0;width:0;height:0}
  .switch .sl{position:absolute;cursor:pointer;inset:0;background:#d3d9e5;border-radius:22px;transition:.2s}
  .switch .sl:before{content:"";position:absolute;height:16px;width:16px;left:3px;top:3px;
    background:#fff;border-radius:50%;transition:.2s;box-shadow:0 1px 2px rgba(0,0,0,.2)}
  .switch input:checked + .sl{background:var(--primary)}
  .switch input:checked + .sl:before{transform:translateX(18px)}

  /* ── 客户端卡片 ── */
  .client-grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(340px,1fr));gap:16px}
  .client{background:#fff;border:1px solid var(--line);border-radius:var(--radius);box-shadow:var(--shadow);overflow:hidden}
  .client .chead{display:flex;align-items:center;gap:10px;padding:14px 16px;border-bottom:1px solid var(--line2)}
  .client .avatar{width:38px;height:38px;border-radius:10px;background:linear-gradient(135deg,#eef2ff,#e6ecfe);
    display:flex;align-items:center;justify-content:center;font-size:17px;flex-shrink:0}
  .client .who{flex:1;min-width:0}
  .client .host{font-weight:650;font-size:14px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
  .client .cid{font-size:11.5px;color:var(--faint);font-family:ui-monospace,monospace}
  .client .cbody{padding:14px 16px}
  .client .sec{font-size:11.5px;color:var(--faint);font-weight:600;letter-spacing:.03em;margin:12px 0 6px;text-transform:uppercase}
  .client .sec:first-child{margin-top:0}
  .client .foot{display:flex;align-items:center;justify-content:space-between;padding:10px 16px;
    background:#fafbfd;border-top:1px solid var(--line2);font-size:12px;color:var(--muted)}

  /* ── 口令弹窗 ── */
  .mask{position:fixed;inset:0;background:rgba(15,23,42,.42);z-index:50;display:none;align-items:center;justify-content:center}
  .mask.show{display:flex}
  .modal{background:#fff;border-radius:14px;box-shadow:0 20px 60px rgba(0,0,0,.25);width:min(420px,92vw);padding:24px}
  .modal h3{margin:0 0 6px;font-size:16px}
  .modal .desc{color:var(--muted);font-size:13px;margin-bottom:16px}

  /* ── Toast ── */
  .toast-wrap{position:fixed;right:20px;bottom:20px;z-index:60;display:flex;flex-direction:column;gap:10px}
  .toast{background:#1a2233;color:#fff;border-radius:10px;padding:12px 16px;font-size:13.5px;
    box-shadow:0 8px 30px rgba(0,0,0,.25);display:flex;align-items:center;gap:10px;
    animation:tin .22s ease;max-width:360px}
  .toast.ok{background:#0e7a3d} .toast.err{background:#b91c1c} .toast.warn{background:#a16207}
  @keyframes tin{from{opacity:0;transform:translateY(8px)}to{opacity:1;transform:none}}
</style>
</head>
<body>

<header class="topbar">
  <div class="brand"><span class="logo">H</span> Harness 中心管理</div>
  <div class="spacer"></div>
  <span class="sync-hint" id="syncHint">—</span>
  <button class="btn ghost" onclick="refreshAll()" title="刷新">⟳ 刷新</button>
  <button class="btn" onclick="openTokenModal()">🔑 管理口令</button>
</header>

<nav class="tabs">
  <button class="tab active" data-view="overview">概览</button>
  <button class="tab" data-view="plugins">插件策略</button>
  <button class="tab" data-view="menu">菜单策略</button>
  <button class="tab" data-view="clients">客户端</button>
</nav>

<main>
  <!-- 概览 -->
  <section id="view-overview" class="view active">
    <div class="kpis" id="kpis"></div>
    <div class="card">
      <div class="card-head">
        <div><h2 class="card-title">客户端健康概览</h2>
        <div class="card-desc">最近同步的客户端状态，点击顶部「客户端」查看完整详情</div></div>
      </div>
      <div id="overviewClients"></div>
    </div>
  </section>

  <!-- 插件策略 -->
  <section id="view-plugins" class="view">
    <div class="card">
      <div class="card-head">
        <div><h2 class="card-title">应装插件清单</h2>
        <div class="card-desc">客户端任一个 profile 装了即满足；未装的客户端会弹通知，点击托盘「安装」即可补齐</div></div>
      </div>
      <div class="bridge-bar" style="background:#f2f5fc;border:1px solid #dbe3f5;border-radius:9px;padding:8px 12px;margin-bottom:14px;font-size:12.5px;display:flex;align-items:center;gap:10px;flex-wrap:wrap">
        <span>🛰 本机管理能力：</span>
        <span id="bridgeState" style="color:var(--muted)">检测中…</span>
        <input class="input" id="bridgePortInput" placeholder="端口" style="width:80px;padding:4px 8px" value="">
        <input class="input" id="bridgeTokenInput" type="password" placeholder="连接 token（托盘开启时通知里显示）" style="width:200px;padding:4px 8px" value="">
        <button class="btn sm" onclick="setBridgePort()">连接</button>
        <span style="color:var(--faint)">（仅连管理页所在电脑的 launcher；token 见管理员本机托盘「开启管理能力」通知）</span>
      </div>
      <div class="row" style="margin-bottom:12px">
        <input class="input" id="newPlugin" placeholder="输入 npm 包名，如 dsh-nested-followups 或 @scope/pkg" onkeydown="if(event.key==='Enter')addPlugin()">
        <button class="btn primary" onclick="addPlugin()">＋ 添加</button>
      </div>
      <div class="row" style="margin-bottom:14px;flex-wrap:wrap;gap:8px">
        <button class="btn" onclick="checkAllSyncStatus()">⟳ 刷新同步状态</button>
        <button class="btn primary" id="syncAllBtn" onclick="syncAllPlugins()">🚀 同步全部未同步</button>
        <span class="sync-hint" id="syncState"></span>
        <span style="color:var(--faint);font-size:12px">同步目标：下方「镜像上传」卡片的 registry</span>
      </div>
      <div id="syncProgress" style="margin-bottom:12px"></div>
      <div class="plugin-cards" id="pluginList"></div>
      <div style="margin-top:16px;display:flex;gap:8px;align-items:center">
        <button class="btn primary" onclick="saveConfig()">保存插件策略</button>
        <span style="font-size:12.5px;color:var(--muted)">修改后需保存，客户端下次轮询（默认 5 分钟）生效</span>
      </div>
    </div>
    <div class="card">
      <div class="card-head">
        <div><h2 class="card-title">客户端默认配置</h2>
        <div class="card-desc">下发给客户端的配置默认值（客户端本地显式设置过的不被覆盖）</div></div>
      </div>
      <div class="row" style="margin-bottom:10px">
        <input class="input" id="cdNpmRegistry" placeholder="npm registry，多个用逗号分隔（如 https://registry.npmmirror.com/, https://registry.npmjs.org/，空=不覆盖）">
      </div>
      <div class="row" style="margin-bottom:10px">
        <input class="input" id="cdGhMirror" placeholder="GitHub 中转前缀，多个用逗号分隔（如 https://ghfast.top/, https://ghproxy.net/，空=不覆盖）">
      </div>
      <div class="row" style="margin-bottom:10px">
        <input class="input" id="cdPort" placeholder="端口（空=不覆盖）" style="max-width:200px">
        <input class="input" id="cdSyncSecs" placeholder="同步间隔秒（>=30，空=不覆盖）" style="max-width:220px">
      </div>
      <div class="row" style="margin-bottom:10px">
        <input class="input" id="cdProfile" placeholder="profile（如 web / matrix，空=不覆盖）" style="max-width:220px">
      </div>
      <div style="margin-bottom:10px;font-size:13px">
        <label><input type="checkbox" id="cdUseSystemNode" style="width:auto"> 客户端优先使用系统 node（主版本≥22 则跳过下载自带 node）</label>
      </div>
      <div style="margin-top:12px"><button class="btn primary" onclick="saveClientDefaults()">保存客户端默认配置</button></div>
    </div>
    <div class="card">
      <div class="card-head">
        <div><h2 class="card-title">内网 registry（镜像上传 + 同步状态）</h2>
        <div class="card-desc">此处配置的 registry 同时用于：① 上方插件清单的「同步状态」检查与「同步」按钮；② 把「应装插件 + 全部依赖」上传（镜像）到该 registry</div></div>
      </div>
      <div class="row" style="margin-bottom:10px">
        <input class="input" id="mirrorRegistry" placeholder="内网 registry（如 https://registry.ict.cmcc）">
        <input class="input" type="password" id="mirrorToken" placeholder="发布 token（NODE_AUTH_TOKEN 值，存服务端，调用时传递）" style="max-width:320px">
      </div>
      <div style="margin-bottom:10px;font-size:12.5px;color:var(--muted)">token 存于服务端 config.json；点击「开始上传」时经管理能力临时传给管理员 launcher（内存使用，不落盘客户端）。</div>
      <div style="margin-top:12px;display:flex;gap:8px;align-items:center">
        <button class="btn primary" onclick="saveMirrorSettings()">保存镜像设置</button>
        <button class="btn" onclick="startMirrorUpload()" id="mirrorStartBtn">🚀 开始上传到内网 registry</button>
        <span class="sync-hint" id="mirrorState"></span>
      </div>
      <div id="mirrorProgress" style="margin-top:12px;font-size:13px"></div>
    </div>
  </section>

  <!-- 菜单策略 -->
  <section id="view-menu" class="view">
    <div class="card">
      <div class="card-head">
        <div><h2 class="card-title">托盘「常用网址」菜单策略</h2>
        <div class="card-desc">统一下发同事托盘里的快捷菜单，启用后覆盖客户端本地菜单展示，关闭后自动回退</div></div>
        <label class="switch" title="启停统一下发">
          <input type="checkbox" id="menuEnabled" onchange="current.managedMenu.enabled=this.checked">
          <span class="sl"></span>
        </label>
      </div>
      <div id="menuList"></div>
      <div class="row" style="margin-top:14px">
        <input class="input" id="newMenuLabel" placeholder="菜单名，如 公司OA">
        <input class="input" id="newMenuUrl" placeholder="https:// 或 http:// 地址" onkeydown="if(event.key==='Enter')addMenuItem()">
        <button class="btn primary" onclick="addMenuItem()">＋ 添加</button>
      </div>
      <div style="margin-top:16px"><button class="btn primary" onclick="saveMenuPolicy()">保存菜单策略</button></div>
    </div>
  </section>

  <!-- 客户端 -->
  <section id="view-clients" class="view">
    <div class="client-grid" id="clientGrid"></div>
  </section>
</main>

<!-- 口令弹窗 -->
<div class="mask" id="tokenMask">
  <div class="modal">
    <h3>管理口令</h3>
    <div class="desc">输入服务端启动时的 <code>--token</code> 值（未设置则留空）。保存后存于本浏览器。</div>
    <div class="field">
      <label>管理口令</label>
      <input class="input" type="password" id="tokenInput" placeholder="如 test123">
    </div>
    <div class="row" style="justify-content:flex-end">
      <button class="btn" onclick="closeTokenModal()">取消</button>
      <button class="btn primary" onclick="saveToken()">保存</button>
    </div>
  </div>
</div>

<div class="toast-wrap" id="toastWrap"></div>

<script src="/admin.js"></script>
</body>
</html>`;
}

// ---------- 启动 ----------
ensureData();
const server = http.createServer((req, res) => {
  route(req, res).catch((e) => {
    try { send(res, 500, { error: e.message }); } catch { /* 连接已断 */ }
  });
});
server.listen(ARGS.port, "0.0.0.0", () => {
  console.log("Harness Launcher 中心服务已启动:");
  console.log("  管理页:      http://<本机IP>:" + ARGS.port + "/admin" + (ARGS.token ? "" : "（未设 token）"));
  console.log("  客户端配置:  http://<本机IP>:" + ARGS.port + "/api/config");
  console.log("  数据目录:    " + ARGS.data);
  if (!ARGS.token) {
    console.log("  警告: 未设置 --token，管理写操作无鉴权，仅建议内网使用。");
  }
});
