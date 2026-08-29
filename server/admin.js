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
  document.getElementById("mirrorRegistry").value=ms.registry||"http://registry.ict.cmcc";
  document.getElementById("mirrorToken").value=ms.tokenValue||"";
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
    if(!/^https?:\/\/\S+$/.test(ms.registry)){ toast("registry 必须是 http(s) 地址","warn"); return; }
    const body={plugins:current.plugins,managedMenu:current.managedMenu,clientDefaults:current.clientDefaults||{},mirrorSettings:ms};
    const r=await fetch("/api/config",{method:"POST",headers:headers(true),body:JSON.stringify(body)});
    const j=await r.json();
    if(!r.ok) throw new Error((j&&j.error)||("HTTP "+r.status));
    current=j; toast("镜像设置已保存（含 token）","ok");
  }catch(e){ toast("保存失败："+esc(e.message),"err"); }
}
async function startMirrorUpload(){
  if(!bridgePort){ toast("请先连接管理员本机管理能力（插件策略页顶部）","warn"); return; }
  const reg=document.getElementById("mirrorRegistry").value.trim()||"http://registry.ict.cmcc";
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
  const bridgeTok=localStorage.getItem("bridgeToken")||"";
  try{
    const r=await fetch("http://127.0.0.1:"+bridgePort+"/api/registry/mirror/progress?token="+encodeURIComponent(bridgeTok),{headers:headers(false)});
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
// 管理能力：只探测「本机」（管理页所在电脑）的 launcher，不猜其他客户端。
// 主动 fetch /api/health 确认真的可用，连不上就明确显示未连接。
async function autoDetectBridge(){
  if(bridgePort) return; // 已手动/自动设置
  const st=document.getElementById("bridgeState");
  // 尝试常用端口（可配置），确认本机管理能力真的开启
  const candidates=[parseInt(localStorage.getItem("bridgePort")||"0",10)||3410, 3410];
  for(const port of candidates){
    try{
      const r=await fetch("http://127.0.0.1:"+port+"/api/health",{headers:headers(false)});
      if(r.ok){
        const j=await r.json();
        if(j&&j.ok){
          bridgePort=port;
          document.getElementById("bridgePortInput").value=port;
          // 恢复已保存的 token
          const saved=localStorage.getItem("bridgeToken")||"";
          if(saved) document.getElementById("bridgeTokenInput").value=saved;
          st.innerHTML='<span style="color:var(--green)">✓ 已连接本机管理能力（端口 '+port+'）</span>';
          return;
        }
      }
    }catch(e){ /* 该端口无服务，继续 */ }
  }
  st.innerHTML='<span style="color:var(--muted)">本机管理能力未连接——同步/上传需管理员在本机 launcher 托盘「管理能力」开启</span>';
}
async function setBridgePort(){
  const v=document.getElementById("bridgePortInput").value.trim();
  const p=parseInt(v,10);
  if(!p||p<1||p>65535){ toast("端口无效","warn"); return; }
  // 验证该端口确实是本机管理能力
  try{
    const r=await fetch("http://127.0.0.1:"+p+"/api/health",{headers:headers(false)});
    const j=await r.json();
    if(!r.ok||!j||!j.ok){ toast("该端口不是有效的管理能力服务","warn"); return; }
  }catch(e){ toast("无法连接该端口（本机管理能力未开启？）","warn"); return; }
  bridgePort=p;
  localStorage.setItem("bridgePort",String(p));
  // 保存连接 token（管理能力鉴权用）
  const tok=document.getElementById("bridgeTokenInput").value.trim();
  if(tok) localStorage.setItem("bridgeToken",tok);
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
    const syncBtn='<button class="btn sm primary sync-btn" id="syncbtn-'+i+'" onclick="syncOnePlugin('+i+')">同步此插件</button>';
    card.className="pcard";
    card.innerHTML='<div class="phead"><span class="pname">'+esc(p)+'</span>'+ver+'</div>'
      +'<div class="pdesc">'+desc+'</div>'
      +'<div class="pmeta">'+src+(home||'')+'</div>'
      +'<div class="pfoot">'+badge+syncBtn+'<button class="btn sm danger" onclick="removePlugin('+i+')">移除</button></div>';
  });
}

// ── 同步状态查询（管理页浏览器直查内网 registry） ──
let syncRegistryCache="";
// 同步目标 registry 单一来源：「镜像上传」卡片的 mirrorRegistry（避免两处配置困惑）
function syncRegistryUrl(){
  const v=document.getElementById("mirrorRegistry").value.trim();
  return v||"http://registry.ict.cmcc";
}
async function checkRegistryStatus(names){
  const reg=syncRegistryUrl().replace(/\/+$/,"");
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
  const btn=document.getElementById("syncbtn-"+i);
  const setBtn=(text,disabled)=>{ if(btn){ btn.textContent=text; btn.disabled=!!disabled; } };
  // 前置条件检查（明确反馈，不只 toast）
  if(!bridgePort){
    toast("❌ 未连接管理员本机管理能力——请在上方「本机管理能力」输入端口并连接","err");
    return;
  }
  const bridgeTok=localStorage.getItem("bridgeToken")||"";
  const name=(current.plugins||[])[i];
  const token=document.getElementById("mirrorToken").value.trim();
  if(!token){
    toast("❌ 未配置发布 token——请滚动到下方「镜像上传」卡片填写","err");
    setBtn("同步此插件",false);
    return;
  }
  const reg=syncRegistryUrl();
  // 点击立即反馈：按钮禁用 + 状态
  setBtn("⏳ 同步中…",true);
  try{
    const ctrl=new AbortController();
    setTimeout(()=>ctrl.abort(),15000); // 15s 超时
    const r=await fetch("http://127.0.0.1:"+bridgePort+"/api/registry/mirror/start?token="+encodeURIComponent(bridgeTok),{
      method:"POST",headers:headers(true),
      body:JSON.stringify({registry:reg,token:token,only:name}),
      signal:ctrl.signal
    });
    const j=await r.json();
    if(!j.ok){
      setBtn("同步此插件",false);
      toast("❌ 同步启动失败："+esc(j.error||("HTTP "+r.status)),"err");
      return;
    }
    toast("🚀 正在同步 "+esc(name)+"（含依赖）…","ok");
    // 立即刷新一次进度 + 定时轮询
    pollMirrorProgress();
    // 轮询期间按钮保持「同步中」
    const t=setInterval(()=>{
      fetch("http://127.0.0.1:"+bridgePort+"/api/registry/mirror/progress?token="+encodeURIComponent(bridgeTok),{headers:headers(false)})
        .then(r=>r.json()).then(j=>{
          const p=(j&&j.progress)||{};
          if(p.state==="done"||p.state==="error"){
            clearInterval(t);
            setBtn("同步此插件",false);
            toast(p.state==="done" ? ("✅ "+esc(name)+" 已同步") : ("❌ 同步失败："+esc((p.error||"").slice(0,120))), p.state==="done"?"ok":"err");
            renderPlugins(); // 刷新徽章
          }
        }).catch(()=>{});
    },2000);
  }catch(e){
    setBtn("同步此插件",false);
    toast("❌ 无法连接管理能力："+esc(e.message),"err");
  }
}
async function syncAllPlugins(){
  const btn=document.getElementById("syncAllBtn");
  if(!bridgePort){ toast("❌ 未连接管理员本机管理能力——请在上方输入端口并连接","err"); return; }
  const bridgeTok=localStorage.getItem("bridgeToken")||"";
  const token=document.getElementById("mirrorToken").value.trim();
  if(!token){ toast("❌ 未配置发布 token——请滚动到「镜像上传」卡片填写","err"); return; }
  const reg=syncRegistryUrl();
  if(btn){ btn.disabled=true; btn.textContent="⏳ 同步中…"; }
  try{
    const ctrl=new AbortController();
    setTimeout(()=>ctrl.abort(),15000);
    const r=await fetch("http://127.0.0.1:"+bridgePort+"/api/registry/mirror/start?token="+encodeURIComponent(bridgeTok),{
      method:"POST",headers:headers(true),
      body:JSON.stringify({registry:reg,token:token}),
      signal:ctrl.signal
    });
    const j=await r.json();
    if(!j.ok){ toast("❌ 同步启动失败："+esc(j.error||""),"err"); if(btn){btn.disabled=false;btn.textContent="🚀 同步全部未同步";} return; }
    toast("🚀 已开始同步全部未同步插件","ok");
    pollMirrorProgress();
  }catch(e){ toast("❌ 无法连接管理能力："+esc(e.message),"err"); if(btn){btn.disabled=false;btn.textContent="🚀 同步全部未同步";} }
}
function addPlugin(){
  const v=document.getElementById("newPlugin").value.trim();
  if(!v){ toast("请输入包名","warn"); return; }
  if(!/^(?:@[a-z0-9-~][a-z0-9-._~]*\/)?[a-z0-9-~][a-z0-9-._~]*$/.test(v)){ toast("包名格式不合法","warn"); return; }
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
  if(!label||!/^https?:\/\/\S+$/i.test(url)){ toast("菜单名非空、地址需 http/https","warn"); return; }
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