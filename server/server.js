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
        <span>🛰 包信息查询：</span>
        <span id="bridgeState" style="color:var(--muted)">检测管理能力…</span>
        <input class="input" id="bridgePortInput" placeholder="本机端口" style="width:90px;padding:4px 8px" value="">
        <button class="btn sm" onclick="setBridgePort()">连接</button>
        <span style="color:var(--faint)">（管理员本机 launcher「管理能力」开启后自动探测；服务端不直连外网）</span>
      </div>
      <div class="row" style="margin-bottom:12px">
        <input class="input" id="newPlugin" placeholder="输入 npm 包名，如 dsh-nested-followups 或 @scope/pkg" onkeydown="if(event.key==='Enter')addPlugin()">
        <button class="btn primary" onclick="addPlugin()">＋ 添加</button>
      </div>
      <div class="row" style="margin-bottom:14px;flex-wrap:wrap;gap:8px">
        <input class="input" id="syncRegistry" placeholder="同步目标 registry（如 https://registry.ict.cmcc）" style="max-width:300px">
        <button class="btn" onclick="checkAllSyncStatus()">⟳ 刷新同步状态</button>
        <button class="btn primary" id="syncAllBtn" onclick="syncAllPlugins()">🚀 同步全部未同步</button>
        <span class="sync-hint" id="syncState"></span>
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
        <div><h2 class="card-title">镜像上传（同步到内网 registry）</h2>
        <div class="card-desc">管理员 launcher 把「应装插件 + 全部依赖」上传到内网 registry（需连接管理能力，管理员机配 NODE_AUTH_TOKEN）</div></div>
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

<script>
let TOKEN = localStorage.getItem("adminToken") || "";
let current = { plugins: [], managedMenu: { enabled: false, quickLinks: [] } };
let latestClients = [];

function headers(j){ const h = j?{"Content-Type":"application/json"}:{}; if(TOKEN) h["X-Admin-Token"]=TOKEN; return h; }
function esc(s){ return String(s).replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;").replace(/"/g,"&quot;").replace(/'/g,"&#39;"); }
function toast(msg, type){
  const w=document.getElementById("toastWrap");
  const t=document.createElement("div"); t.className="toast "+(type||"");
  t.innerHTML=msg; w.appendChild(t);
  setTimeout(()=>{ t.style.opacity="0"; t.style.transition="opacity .3s"; setTimeout(()=>t.remove(),300); },2600);
}
function fmtTime(iso){ if(!iso) return "—"; const d=new Date(iso); const now=new Date();
  const diff=Math.round((now-d)/1000);
  if(diff<60) return "刚刚"; if(diff<3600) return Math.floor(diff/60)+" 分钟前";
  if(diff<86400) return Math.floor(diff/3600)+" 小时前";
  return d.toLocaleDateString()+" "+d.toLocaleTimeString([], {hour:"2-digit",minute:"2-digit"});
}

// ── Tab 切换 ──
document.querySelectorAll(".tab").forEach(t=>t.addEventListener("click",()=>{
  document.querySelectorAll(".tab").forEach(x=>x.classList.remove("active"));
  document.querySelectorAll(".view").forEach(x=>x.classList.remove("active"));
  t.classList.add("active");
  document.getElementById("view-"+t.dataset.view).classList.add("active");
}));

// ── 口令 ──
function openTokenModal(){ document.getElementById("tokenInput").value=TOKEN; document.getElementById("tokenMask").classList.add("show"); }
function closeTokenModal(){ document.getElementById("tokenMask").classList.remove("show"); }
document.getElementById("tokenMask").addEventListener("click",e=>{ if(e.target.id==="tokenMask") closeTokenModal(); });
function saveToken(){
  TOKEN=document.getElementById("tokenInput").value.trim();
  localStorage.setItem("adminToken",TOKEN);
  closeTokenModal(); toast("管理口令已保存","ok");
  refreshAll();
}

// ── 数据加载 ──
async function loadConfig(){
  try{
    const r=await fetch("/api/config"); const j=await r.json();
    if(!r.ok){ throw new Error((j&&j.error)||("HTTP "+r.status)); }
    current=j; current.managedMenu=current.managedMenu||{enabled:false,quickLinks:[]};
    current.clientDefaults=current.clientDefaults||{};
    renderPlugins(); renderMenuPolicy(); renderClientDefaults();
  }catch(e){ toast("加载配置失败："+esc(e.message),"err"); }
}
function renderClientDefaults(){
  const cd=current.clientDefaults||{};
  document.getElementById("cdNpmRegistry").value=(cd.npmRegistry||[]).join(", ");
  document.getElementById("cdGhMirror").value=(cd.ghMirrorPrefix||[]).join(", ");
  document.getElementById("cdPort").value=cd.port||"";
  document.getElementById("cdSyncSecs").value=cd.syncIntervalSecs||"";
  document.getElementById("cdProfile").value=cd.profile||"";
  document.getElementById("cdUseSystemNode").checked=!!cd.useSystemNode;
  const ms=current.mirrorSettings||{};
  document.getElementById("mirrorRegistry").value=ms.registry||"https://registry.ict.cmcc";
  document.getElementById("mirrorToken").value=ms.tokenValue||"";
  document.getElementById("syncRegistry").value=ms.registry||"https://registry.ict.cmcc";
}
async function saveClientDefaults(){
  try{
    const cd={};
    // 逗号分隔输入 → 数组（多源）
    const toList=(s)=>s.split(",").map(x=>x.trim()).filter(Boolean);
    const npm=document.getElementById("cdNpmRegistry").value.trim();
    const gh=document.getElementById("cdGhMirror").value.trim();
    const port=document.getElementById("cdPort").value.trim();
    const sync=document.getElementById("cdSyncSecs").value.trim();
    const prof=document.getElementById("cdProfile").value.trim();
    const npmList=toList(npm), ghList=toList(gh);
    if(npmList.length) cd.npmRegistry=npmList;
    if(ghList.length) cd.ghMirrorPrefix=ghList;
    if(port){ const p=parseInt(port,10); if(p<1||p>65535){ toast("端口无效","warn"); return; } cd.port=p; }
    if(sync){ const s=parseInt(sync,10); if(s<30){ toast("同步间隔需 >=30","warn"); return; } cd.syncIntervalSecs=s; }
    if(prof) cd.profile=prof;
    cd.useSystemNode=document.getElementById("cdUseSystemNode").checked;
    const body={plugins:current.plugins,managedMenu:current.managedMenu,clientDefaults:cd};
    const r=await fetch("/api/config",{method:"POST",headers:headers(true),body:JSON.stringify(body)});
    const j=await r.json();
    if(!r.ok) throw new Error((j&&j.error)||("HTTP "+r.status));
    current=j; current.clientDefaults=current.clientDefaults||{};
    renderClientDefaults(); toast("客户端默认配置已保存","ok");
  }catch(e){ toast("保存失败："+esc(e.message),"err"); }
}

// ── 镜像上传 ──
async function saveMirrorSettings(){
  try{
    const ms={
      registry:document.getElementById("mirrorRegistry").value.trim(),
      tokenValue:document.getElementById("mirrorToken").value.trim(),
    };
    if(!/^https?:\\/\\/\\S+$/.test(ms.registry)){ toast("registry 必须是 http(s) 地址","warn"); return; }
    const body={plugins:current.plugins,managedMenu:current.managedMenu,clientDefaults:current.clientDefaults||{},mirrorSettings:ms};
    const r=await fetch("/api/config",{method:"POST",headers:headers(true),body:JSON.stringify(body)});
    const j=await r.json();
    if(!r.ok) throw new Error((j&&j.error)||("HTTP "+r.status));
    current=j; toast("镜像设置已保存（含 token）","ok");
  }catch(e){ toast("保存失败："+esc(e.message),"err"); }
}
async function startMirrorUpload(){
  if(!bridgePort){ toast("请先连接管理员本机管理能力（插件策略页顶部）","warn"); return; }
  const reg=document.getElementById("mirrorRegistry").value.trim()||"https://registry.ict.cmcc";
  const token=document.getElementById("mirrorToken").value.trim();
  if(!token){ toast("请先配置发布 token（镜像设置）","warn"); return; }
  try{
    // token 经 POST body 传递（不进 URL/日志）
    const r=await fetch("http://127.0.0.1:"+bridgePort+"/api/registry/mirror/start",{
      method:"POST",headers:headers(true),
      body:JSON.stringify({registry:reg,token:token})
    });
    const j=await r.json();
    if(!j.ok){ toast("启动失败："+esc(j.error||""),"err"); return; }
    toast("上传已开始，请查看进度","ok");
    pollMirrorProgress();
  }catch(e){ toast("无法连接管理员管理能力："+esc(e.message),"err"); }
}
let mirrorPollTimer=null;
async function pollMirrorProgress(){
  if(!bridgePort) return;
  try{
    const r=await fetch("http://127.0.0.1:"+bridgePort+"/api/registry/mirror/progress",{headers:headers(false)});
    const j=await r.json();
    if(!j.ok) return;
    const p=j.progress||{};
    const el=document.getElementById("mirrorProgress");
    const st=document.getElementById("mirrorState");
    // 同步工具栏进度显示
    const spEl=document.getElementById("syncProgress");
    const ssEl=document.getElementById("syncState");
    if(p.state==="running"){
      el.innerHTML='<div style="background:#eef4ff;border:1px solid #cfe0ff;border-radius:8px;padding:10px 14px">'+
        '⏳ 上传中：<b>'+esc(p.current_pkg||"…")+'</b><br>'+
        '进度：'+p.done_pkgs+'/'+p.total_pkgs+' 个包（应装 '+p.total_plugins+' 个插件）'+
        '</div>';
      st.innerHTML='<span style="color:var(--amber)">进行中…</span>';
      document.getElementById("mirrorStartBtn").disabled=true;
      document.getElementById("syncAllBtn").disabled=true;
      if(spEl) spEl.innerHTML='<div style="background:#eef4ff;border:1px solid #cfe0ff;border-radius:8px;padding:8px 12px;font-size:13px">'+
        '⏳ 同步中：'+p.done_pkgs+'/'+p.total_pkgs+' 个包 · 当前 '+esc(p.current_pkg||"…")+'</div>';
      if(ssEl) ssEl.innerHTML='<span style="color:var(--amber)">同步中…</span>';
      if(mirrorPollTimer) clearTimeout(mirrorPollTimer);
      mirrorPollTimer=setTimeout(pollMirrorProgress,3000);
    } else if(p.state==="done"){
      el.innerHTML='<div style="background:#e8f7ee;border:1px solid #b7e3c8;border-radius:8px;padding:10px 14px">'+
        '✅ 上传完成：'+p.done_pkgs+'/'+p.total_pkgs+' 个包已同步到 '+esc(p.registry||"")+
        '</div>';
      st.innerHTML='<span style="color:var(--green)">已完成</span>';
      document.getElementById("mirrorStartBtn").disabled=false;
      document.getElementById("syncAllBtn").disabled=false;
      if(spEl) spEl.innerHTML='<div style="background:#e8f7ee;border:1px solid #b7e3c8;border-radius:8px;padding:8px 12px;font-size:13px">'+
        '✅ 同步完成：'+p.done_pkgs+'/'+p.total_pkgs+' 个包已同步</div>';
      if(ssEl) ssEl.innerHTML='<span style="color:var(--green)">已完成</span>';
      // 完成后刷新插件同步状态徽章
      renderPlugins();
    } else if(p.state==="error"){
      el.innerHTML='<div style="background:#fdeaea;border:1px solid #f5c6c6;border-radius:8px;padding:10px 14px">'+
        '❌ 上传出错：'+esc((p.error||"").slice(0,300))+
        '</div>';
      st.innerHTML='<span style="color:var(--red)">出错</span>';
      document.getElementById("mirrorStartBtn").disabled=false;
      document.getElementById("syncAllBtn").disabled=false;
      if(spEl) spEl.innerHTML='<div style="background:#fdeaea;border:1px solid #f5c6c6;border-radius:8px;padding:8px 12px;font-size:13px">'+
        '❌ 同步出错：'+esc((p.error||"").slice(0,200))+'</div>';
      if(ssEl) ssEl.innerHTML='<span style="color:var(--red)">出错</span>';
    }
  }catch(e){ /* 连接断开忽略 */ }
}
async function loadStatus(){
  try{
    const r=await fetch("/api/status",{headers:headers(false)}); const j=await r.json();
    if(!r.ok){
      if(r.status===403){ toast("管理口令错误或未设置，点右上角「管理口令」配置","warn"); }
      else throw new Error((j&&j.error)||("HTTP "+r.status));
      return;
    }
    latestClients=j.clients||[];
    renderKpis(); renderOverviewClients(); renderClients();
    autoDetectBridge();
    const t=latestClients.length? ("上次更新 " + fmtTime(latestClients[0].lastSyncAt)) : "等待客户端上报";
    document.getElementById("syncHint").innerHTML="<b>"+latestClients.length+"</b> 台客户端 · "+t;
  }catch(e){ toast("加载客户端失败："+esc(e.message),"err"); }
}
// 管理能力自动探测：从客户端上报的 bridgeStatus 找本机（hostname 匹配）或任一开启的
async function autoDetectBridge(){
  if(bridgePort) return; // 已手动/自动设置
  const st=document.getElementById("bridgeState");
  const self=latestClients.find(c=>c.bridgeStatus&&c.bridgeStatus.enabled);
  if(self&&self.bridgeStatus.port){
    bridgePort=self.bridgeStatus.port;
    document.getElementById("bridgePortInput").value=bridgePort;
    st.innerHTML='<span style="color:var(--green)">已连接 '+esc(self.hostname||"管理员机")+'（端口 '+bridgePort+'）</span>';
  } else {
    st.innerHTML='<span style="color:var(--muted)">未检测到开启的管理能力——请管理员在 launcher 托盘开启，或手动输入端口</span>';
  }
}
function setBridgePort(){
  const v=document.getElementById("bridgePortInput").value.trim();
  const p=parseInt(v,10);
  if(!p||p<1||p>65535){ toast("端口无效","warn"); return; }
  bridgePort=p;
  localStorage.setItem("bridgePort",String(p));
  // 清缓存强制刷新插件信息
  Object.keys(pluginMetaCache).forEach(k=>delete pluginMetaCache[k]);
  renderPlugins();
  toast("已连接本机管理能力（端口 "+p+"）","ok");
}
function refreshAll(){ loadConfig(); loadStatus(); toast("已刷新","ok"); }

// ── 插件策略 ──
const pluginMetaCache = {}; // name -> meta
let bridgePort = null; // 管理员本机管理能力端口（自动探测/手动设置）
async function fetchPluginMetas(names){
  const need = names.filter(n=>!pluginMetaCache[n]);
  if(need.length){
    // 优先走管理员本机管理能力（外网代理网关）：服务端不直接出外网
    let got = false;
    if(bridgePort){
      try{
        const r=await fetch("http://127.0.0.1:"+bridgePort+"/api/registry/meta?name="+encodeURIComponent(need.join(","))+"&token="+encodeURIComponent(localStorage.getItem("bridgeToken")||""),{headers:headers(false)});
        const j=await r.json();
        if(j.ok){
          // 本 API 单包查询：逐个补齐
          const meta=j.meta;
          if(meta && meta.name){ pluginMetaCache[meta.name]=meta; got=true; }
        }
      }catch(e){ /* 本地 API 不可达，降级服务端 */ }
    }
    if(!got){
      // 降级：服务端直查（若服务端有网）
      try{
        const r=await fetch("/api/plugins/meta?names="+encodeURIComponent(need.join(",")),{headers:headers(false)});
        const j=await r.json();
        (j.plugins||[]).forEach(m=>{ pluginMetaCache[m.name]=m; });
      }catch(e){ /* 拉取失败：卡片显示占位 */ }
    }
  }
  return names.map(n=>pluginMetaCache[n]).filter(Boolean);
}
async function renderPlugins(){
  const el=document.getElementById("pluginList");
  const names=current.plugins||[];
  if(!names.length){ el.innerHTML='<div class="empty">暂无应装插件 —— 所有客户端视为插件齐全</div>'; return; }
  // 先渲染占位（loading），再填充元信息 + 同步状态
  el.innerHTML=names.map((p,i)=>'<div class="pcard loading" id="pcard-'+i+'">'
    +'<div class="phead"><span class="pname">'+esc(p)+'</span><span class="pver">…</span></div>'
    +'<div class="pdesc">正在查询…</div>'
    +'<div class="pfoot"><button class="btn sm danger" onclick="removePlugin('+i+')">移除</button></div>'
    +'</div>').join("");
  const metas=await fetchPluginMetas(names);
  // 查询内网 registry 同步状态（浏览器直查，registry CORS *）
  const syncStates=await checkRegistryStatus(names);
  names.forEach((p,i)=>{
    const card=document.getElementById("pcard-"+i);
    if(!card) return;
    const m=metas.find(x=>x.name===p);
    const ss=syncStates[p]||{state:"checking"};
    const ver=m&&m.latest?'<span class="pver">v'+esc(m.latest)+'</span>':'<span class="pver">?</span>';
    const src=m&&m.registry?'<span class="src">'+esc(m.registry)+'</span>':'';
    const desc=m&&m.description?esc(m.description):'<span class="missing">（无描述）</span>';
    const home=m&&m.homepage?'<a href="'+esc(m.homepage)+'" target="_blank" rel="noopener">主页 ↗</a>':'';
    // 同步状态徽章
    let badge='';
    if(ss.state==="synced") badge='<span class="sync-badge synced">✓ 已同步 v'+esc(ss.version)+'</span>';
    else if(ss.state==="unsynced") badge='<span class="sync-badge unsynced">⚠ 未同步</span>';
    else badge='<span class="sync-badge checking">查询中…</span>';
    // 同步按钮（未同步或已同步都可点，重新同步）
    const syncBtn='<button class="btn sm primary sync-btn" onclick="syncOnePlugin('+i+')">同步此插件</button>';
    card.className="pcard";
    card.innerHTML='<div class="phead"><span class="pname">'+esc(p)+'</span>'+ver+'</div>'
      +'<div class="pdesc">'+desc+'</div>'
      +'<div class="pmeta">'+src+(home||'')+'</div>'
      +'<div class="pfoot">'+badge+syncBtn+'<button class="btn sm danger" onclick="removePlugin('+i+')">移除</button></div>';
  });
}

// ── 同步状态查询（管理页浏览器直查内网 registry） ──
let syncRegistryCache="";
function syncRegistryUrl(){
  const v=document.getElementById("syncRegistry").value.trim();
  return v||"https://registry.ict.cmcc";
}
async function checkRegistryStatus(names){
  const reg=syncRegistryUrl().replace(/\\/+$/,"");
  const out={};
  await Promise.all(names.map(async (p)=>{
    out[p]={state:"checking"};
    try{
      const r=await fetch(reg+"/"+encodeURIComponent(p),{headers:headers(false)});
      if(r.ok){
        const j=await r.json();
        out[p]={state:"synced",version:(j["dist-tags"]&&j["dist-tags"].latest)||"?"};
      } else if(r.status===404){
        out[p]={state:"unsynced"};
      } else {
        out[p]={state:"checking"};
      }
    }catch(e){ out[p]={state:"checking"}; }
  }));
  return out;
}
async function checkAllSyncStatus(){
  const names=current.plugins||[];
  if(!names.length){ toast("无应装插件","warn"); return; }
  toast("正在检查同步状态…","ok");
  renderPlugins();
}
async function syncOnePlugin(i){
  if(!bridgePort){ toast("请先连接管理员本机管理能力","warn"); return; }
  const name=(current.plugins||[])[i];
  const token=document.getElementById("mirrorToken").value.trim();
  if(!token){ toast("请先配置发布 token（下方镜像设置）","warn"); return; }
  const reg=syncRegistryUrl();
  try{
    const r=await fetch("http://127.0.0.1:"+bridgePort+"/api/registry/mirror/start",{
      method:"POST",headers:headers(true),
      body:JSON.stringify({registry:reg,token:token,only:name})
    });
    const j=await r.json();
    if(!j.ok){ toast("启动失败："+esc(j.error||""),"err"); return; }
    toast("正在同步 "+esc(name)+"（含依赖）…","ok");
    pollMirrorProgress();
  }catch(e){ toast("无法连接管理能力："+esc(e.message),"err"); }
}
async function syncAllPlugins(){
  if(!bridgePort){ toast("请先连接管理员本机管理能力","warn"); return; }
  const token=document.getElementById("mirrorToken").value.trim();
  if(!token){ toast("请先配置发布 token（下方镜像设置）","warn"); return; }
  const reg=syncRegistryUrl();
  try{
    const r=await fetch("http://127.0.0.1:"+bridgePort+"/api/registry/mirror/start",{
      method:"POST",headers:headers(true),
      body:JSON.stringify({registry:reg,token:token})
    });
    const j=await r.json();
    if(!j.ok){ toast("启动失败："+esc(j.error||""),"err"); return; }
    toast("已开始同步全部未同步插件","ok");
    pollMirrorProgress();
  }catch(e){ toast("无法连接管理能力："+esc(e.message),"err"); }
}
function addPlugin(){
  const v=document.getElementById("newPlugin").value.trim();
  if(!v){ toast("请输入包名","warn"); return; }
  if(!/^(?:@[a-z0-9-~][a-z0-9-._~]*\\/)?[a-z0-9-~][a-z0-9-._~]*$/.test(v)){ toast("包名格式不合法","warn"); return; }
  if(!current.plugins.includes(v)) current.plugins.push(v);
  document.getElementById("newPlugin").value=""; renderPlugins();
}
function removePlugin(i){ current.plugins.splice(i,1); renderPlugins(); }
async function saveConfig(){
  try{
    const r=await fetch("/api/config",{method:"POST",headers:headers(true),body:JSON.stringify({plugins:current.plugins})});
    const j=await r.json();
    if(!r.ok) throw new Error((j&&j.error)||("HTTP "+r.status));
    current=j; current.managedMenu=current.managedMenu||{enabled:false,quickLinks:[]};
    renderPlugins(); toast("插件策略已保存（"+current.plugins.length+" 个）","ok");
  }catch(e){ toast("保存失败："+esc(e.message),"err"); }
}

// ── 菜单策略 ──
function renderMenuPolicy(){
  document.getElementById("menuEnabled").checked=!!current.managedMenu.enabled;
  const el=document.getElementById("menuList");
  const links=current.managedMenu.quickLinks||[];
  el.innerHTML=links.length
    ? links.map((q,i)=>'<div class="menu-item">'
        +'<span class="idx">'+(i+1)+'</span>'
        +'<input class="input" value="'+esc(q.label)+'" onchange="editMenuLabel('+i+',this.value)" placeholder="菜单名">'
        +'<input class="input" value="'+esc(q.url)+'" onchange="editMenuUrl('+i+',this.value)" placeholder="http(s)://地址">'
        +'<button class="btn sm ghost" onclick="moveMenuItem('+i+',-1)" title="上移">↑</button>'
        +'<button class="btn sm ghost" onclick="moveMenuItem('+i+',1)" title="下移">↓</button>'
        +'<button class="btn sm danger" onclick="removeMenuItem('+i+')" title="删除">✕</button>'
      +'</div>').join("")
    : '<div class="empty">暂无菜单项 —— 启用策略后客户端仍显示自己的本地菜单</div>';
}
function addMenuItem(){
  const label=document.getElementById("newMenuLabel").value.trim();
  const url=document.getElementById("newMenuUrl").value.trim();
  if(!label||!/^https?:\\/\\/\\S+$/i.test(url)){ toast("菜单名非空、地址需 http/https","warn"); return; }
  current.managedMenu.quickLinks=current.managedMenu.quickLinks||[];
  current.managedMenu.quickLinks.push({label,url});
  document.getElementById("newMenuLabel").value=""; document.getElementById("newMenuUrl").value="";
  renderMenuPolicy();
}
function removeMenuItem(i){ current.managedMenu.quickLinks.splice(i,1); renderMenuPolicy(); }
function moveMenuItem(i,d){ const a=current.managedMenu.quickLinks; const j=i+d;
  if(j<0||j>=a.length) return; [a[i],a[j]]=[a[j],a[i]]; renderMenuPolicy(); }
function editMenuLabel(i,v){ current.managedMenu.quickLinks[i].label=v; }
function editMenuUrl(i,v){ current.managedMenu.quickLinks[i].url=v; }
async function saveMenuPolicy(){
  try{
    current.managedMenu.enabled=document.getElementById("menuEnabled").checked;
    const body={plugins:current.plugins,managedMenu:current.managedMenu};
    const r=await fetch("/api/config",{method:"POST",headers:headers(true),body:JSON.stringify(body)});
    const j=await r.json();
    if(!r.ok) throw new Error((j&&j.error)||("HTTP "+r.status));
    current=j; current.managedMenu=current.managedMenu||{enabled:false,quickLinks:[]};
    renderMenuPolicy(); toast("菜单策略已保存","ok");
  }catch(e){ toast("保存失败："+esc(e.message),"err"); }
}

// ── 客户端 ──
function healthOf(c){
  if(c.offline) return {k:"bad",txt:"离线",dot:"bad"};
  if((c.pending||[]).length>0) return {k:"warn",txt:"缺插件",dot:"warn"};
  const pe=!!(current.managedMenu&&current.managedMenu.enabled);
  if(pe&&!c.menuApplied) return {k:"warn",txt:"菜单未应用",dot:"warn"};
  return {k:"ok",txt:"正常",dot:"ok"};
}
function chipPlugins(list){
  if(!list||!list.length) return '<span class="chip dim">未上报</span>';
  return list.map(p=>{
    const web=p.client?'<span class="web">web</span>':'';
    const pf=p.profile?'<span class="pf">'+esc(p.profile)+'</span>':'';
    const tip=esc((p.description||"")+"  ·  "+p.name+"@"+(p.version||"?")+"  ·  profile: "+(p.profile||"-"));
    return '<span class="chip" title="'+tip+'">'+esc(p.name)+'<span class="ver">@'+esc(p.version||"?")+'</span>'+pf+web+'</span>';
  }).join("");
}
function renderKpis(){
  const cs=latestClients;
  const online=cs.filter(c=>!c.offline).length;
  const missing=cs.filter(c=>(c.pending||[]).length>0).length;
  const pe=!!(current.managedMenu&&current.managedMenu.enabled);
  const notApplied=pe?cs.filter(c=>!c.offline&&!c.menuApplied).length:null;
  document.getElementById("kpis").innerHTML=
    kpi(cs.length,"客户端总数","🖥","b")+
    kpi(online,"在线","🟢","g")+
    kpi(missing,"缺插件","⚠️","r")+
    (notApplied!==null?kpi(notApplied,"菜单未应用","🔧","a"):"");
}
function kpi(n,label,ico,cls){ return '<div class="kpi '+cls+'"><div class="ico">'+ico+'</div><div><div class="num">'+n+'</div><div class="lbl">'+label+'</div></div></div>'; }
function renderOverviewClients(){
  const el=document.getElementById("overviewClients");
  if(!latestClients.length){ el.innerHTML='<div class="empty">尚无客户端上报 —— 同事端配置 serverUrl 后会自动同步到这里</div>'; return; }
  el.innerHTML='<div class="chips">'+latestClients.slice(0,12).map(c=>{
    const h=healthOf(c);
    return '<span class="chip dim"><span class="dot '+h.dot+'"></span>'+esc(c.hostname||c.clientId.slice(0,8))+'</span>';
  }).join("")+'</div>';
}
function renderClients(){
  const el=document.getElementById("clientGrid");
  if(!latestClients.length){ el.innerHTML='<div class="card" style="grid-column:1/-1"><div class="empty">尚无客户端上报 —— 同事端配置 serverUrl 后会自动同步到这里</div></div>'; return; }
  const pe=!!(current.managedMenu&&current.managedMenu.enabled);
  el.innerHTML=latestClients.map(c=>{
    const h=healthOf(c);
    const pend=(c.pending||[]).map(p=>'<span class="chip pending">'+esc(p)+'</span>').join("")||'<span class="chip dim">无</span>';
    const menu=(c.menu||[]).map(m=>'<span class="chip">'+esc(m.label)+'</span>').join("")||'<span class="chip dim">无</span>';
    const applied = c.offline ? '<span class="status gray"><span class="dot gray"></span>离线未知</span>'
      : pe ? (c.menuApplied?'<span class="status ok"><span class="dot ok"></span>已应用</span>':'<span class="status warn"><span class="dot warn"></span>未应用</span>')
      : '<span class="status gray"><span class="dot gray"></span>策略关闭</span>';
    const profs=(c.profiles||[]).map(p=>'<span class="chip dim">'+esc(p)+'</span>').join("")||'<span class="chip dim">—</span>';
    return '<div class="client">'
      +'<div class="chead"><div class="avatar">🖥</div>'
      +'<div class="who"><div class="host">'+esc(c.hostname||"未命名")+'</div><div class="cid">'+esc(c.clientId||"")+'</div></div>'
      +'<span class="status '+h.k+'"><span class="dot '+h.dot+'"></span>'+h.txt+'</span></div>'
      +'<div class="cbody">'
      +'<div class="sec">插件（'+(c.plugins||[]).length+'）</div><div class="chips">'+chipPlugins(c.plugins)+'</div>'
      +'<div class="sec">待装</div><div class="chips">'+pend+'</div>'
      +'<div class="sec">托盘菜单</div><div class="chips">'+menu+'</div>'
      +'<div class="sec">Profile</div><div class="chips">'+profs+'</div>'
      +'</div>'
      +'<div class="foot"><span>'+applied+'</span>'
      +'<span title="'+esc(c.lastSyncAt||"")+'">'+fmtTime(c.lastSyncAt)+'</span></div>'
      +'</div>';
  }).join("");
}

// ── 启动 ──
refreshAll();
setInterval(loadStatus, 15000);
</script>
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
