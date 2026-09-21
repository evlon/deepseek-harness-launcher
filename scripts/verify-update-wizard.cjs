/**
 * 向导渲染验证（临时测试脚本，不入库）：
 * 用最小 DOM stub 真实执行 console.rs 内嵌的向导 JS，
 * 断言「计划区 / 步骤 / 历史 / 失败结论」确实渲染出来。
 *
 * 目的：区分「字符串进了二进制」与「界面真的会显示」——
 * 后者才是 Q1 的验收点。
 */
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const SRC = path.join(__dirname, '..', 'src-tauri', 'src', 'console.rs');
const src = fs.readFileSync(SRC, 'utf8');
const m = src.match(/let html = r#"([\s\S]*?)"#\.to_string\(\)/);
if (!m) throw new Error('未能从 console.rs 提取 HTML');
const scriptMatch = m[1].match(/<script>([\s\S]*?)<\/script>/);
if (!scriptMatch) throw new Error('未找到 <script>');
let js = scriptMatch[1];

// ── 最小 DOM stub ────────────────────────────────────────────────
function makeEl(id) {
  return {
    id,
    _text: '', _html: '',
    style: {},
    scrollTop: 0, scrollHeight: 100,
    get textContent() { return this._text; },
    set textContent(v) { this._text = String(v); },
    get innerHTML() { return this._html; },
    set innerHTML(v) { this._html = String(v); },
  };
}
const els = {};
const ids = ['opLabel', 'opPlan', 'opStep', 'opPercent', 'opBar', 'opSteps',
  'opLog', 'opStatus', 'opHistWrap', 'opHist', 'opHistSummary'];
for (const id of ids) els[id] = makeEl(id);

const document = { getElementById: (id) => els[id] || null };
// fetch：/ping 返回 pong；/state、/history 由测试注入
let stateJson = 'null', historyJson = '[]';
const fetch = (url) => {
  if (url.endsWith('/ping')) return Promise.resolve({ text: () => Promise.resolve('pong') });
  if (url.endsWith('/state')) return Promise.resolve({ text: () => Promise.resolve(stateJson) });
  if (url.endsWith('/history')) return Promise.resolve({ text: () => Promise.resolve(historyJson) });
  return Promise.reject(new Error('unknown ' + url));
};
let intervals = [];
const setInterval = (fn, ms) => { intervals.push(fn); return intervals.length; };

// 注入初始状态（模拟 Rust 端 escape 后替换占位符）
const INITIAL_OP = {
  id: 'plugin-install', label: '更新插件', state: 'running',
  current_step: '正在安装 dsh-himarket…',
  steps: [
    { label: '安装插件', state: 'running' },
    { label: '重启 Harness 使其生效', state: 'pending' },
  ],
  log: ['[开始] 更新插件', '[计划] dsh-himarket：0.1.7 → 0.1.8'],
  result: '',
  details: ['dsh-himarket：0.1.7 → 0.1.8', '目标 profile：matrix', '安装完成后将自动重启 Harness 使新版本生效'],
  started_at: '2026-09-21 12:03:34', finished_at: '',
};
js = js.replace('__INITIAL_STATE__', JSON.stringify(INITIAL_OP).replace(/\\/g, '\\\\').replace(/`/g, '\\`'));
js = js.replace('__INITIAL_HISTORY__', '[]');

const sandbox = { document, fetch, setInterval, console, JSON, String, Math, Error };
vm.createContext(sandbox);
vm.runInContext(js, sandbox);

// ── 断言 ─────────────────────────────────────────────────────────
let pass = 0, fail = 0;
function check(name, cond, extra) {
  if (cond) { pass++; console.log('  ✓ ' + name); }
  else { fail++; console.log('  ✗ ' + name + (extra ? ' — ' + extra : '')); }
}

console.log('\n【① 计划区：让用户看到「将要更新什么」】');
check('计划区可见', els.opPlan.style.display === 'block', 'display=' + els.opPlan.style.display);
check('显示「将要执行」标题', els.opPlan.innerHTML.includes('将要执行'));
check('显示版本变化 0.1.7 → 0.1.8', els.opPlan.innerHTML.includes('0.1.7 → 0.1.8'));
check('显示目标 profile', els.opPlan.innerHTML.includes('matrix'));
check('提示会自动重启', els.opPlan.innerHTML.includes('自动重启'));

console.log('\n【② 步骤区：逐步显示进行到哪一步】');
check('步骤区有内容', els.opSteps.innerHTML.length > 0);
check('含「安装插件」', els.opSteps.innerHTML.includes('安装插件'));
check('含「重启 Harness 使其生效」', els.opSteps.innerHTML.includes('重启 Harness 使其生效'));
check('当前步骤标为 running(⏳)', els.opSteps.innerHTML.includes('⏳'));
check('后续步骤标为 pending(○)', els.opSteps.innerHTML.includes('○'));

console.log('\n【③ 状态区：进行中有明确提示】');
check('标题为「更新插件」', els.opLabel.textContent === '更新插件');
check('显示当前动作', els.opLabel.textContent.length > 0 && els.opStep.textContent.includes('正在安装'));
check('状态为「进行中…」', els.opStatus.textContent.includes('进行中'), els.opStatus.textContent);

// ── 模拟推进到「完成」 ────────────────────────────────────────────
console.log('\n【④ 完成态：明确告知已生效】');
stateJson = JSON.stringify({
  ...INITIAL_OP, state: 'done', current_step: '完成',
  steps: [{ label: '安装插件', state: 'done' }, { label: '重启 Harness 使其生效', state: 'done' }],
  result: 'dsh-himarket 已就绪，Harness 已自动重启生效',
  finished_at: '2026-09-21 12:04:30',
});
intervals.forEach((fn) => fn());
setTimeout(() => {
  check('进度条 100%', els.opBar.style.width === '100%', 'width=' + els.opBar.style.width);
  check('状态显示完成', els.opStatus.textContent.includes('完成'), els.opStatus.textContent);
  check('结论含「已自动重启生效」', els.opStatus.textContent.includes('已自动重启生效'));
  check('状态颜色为绿色', els.opStatus.style.color.includes('green'), els.opStatus.style.color);

  // ── 模拟失败态 ──────────────────────────────────────────────────
  console.log('\n【⑤ 失败态：错误必须可见（不再静默）】');
  stateJson = JSON.stringify({
    ...INITIAL_OP, state: 'failed', current_step: '失败',
    steps: [{ label: '安装插件', state: 'failed' }],
    result: 'PLUGIN_INSTALL_FAILED: dsh-himarket（exit=1）：[ERR_PNPM_UNEXPECTED_VIRTUAL_STORE] Unexpected virtual store location',
    finished_at: '2026-09-21 12:03:38',
  });
  intervals.forEach((fn) => fn());
  setTimeout(() => {
    check('状态显示失败', els.opStatus.textContent.includes('失败'));
    check('失败原因可见（不再是「详情见日志」）',
      els.opStatus.textContent.includes('ERR_PNPM_UNEXPECTED_VIRTUAL_STORE'),
      els.opStatus.textContent.slice(0, 120));
    check('状态颜色为红色', els.opStatus.style.color.includes('red'));

    // ── 历史区 ────────────────────────────────────────────────────
    console.log('\n【⑥ 历史区：事后可回看（旧实现被覆盖丢失）】');
    historyJson = JSON.stringify([
      { id: 'plugin-install', label: '安装插件', state: 'failed', result: 'PLUGIN_INSTALL_FAILED', details: ['dsh-himarket：0.1.7 → 0.1.8'], started_at: '2026-09-21 12:03:34', finished_at: '2026-09-21 12:03:38' },
      { id: 'sync', label: '同步', state: 'done', result: '成功', details: [], started_at: '2026-09-21 12:03:23', finished_at: '2026-09-21 12:03:24' },
    ]);
    intervals.forEach((fn) => fn());
    setTimeout(() => {
      check('历史区可见', els.opHistWrap.style.display === 'block', 'display=' + els.opHistWrap.style.display);
      check('历史含失败记录', els.opHist.innerHTML.includes('安装插件') && els.opHist.innerHTML.includes('✗'));
      check('历史含成功记录', els.opHist.innerHTML.includes('同步') && els.opHist.innerHTML.includes('✓'));
      check('历史含版本详情', els.opHist.innerHTML.includes('0.1.7 → 0.1.8'));
      check('历史含时间戳', els.opHist.innerHTML.includes('12:03:38'));

      // ── XSS 防护 ────────────────────────────────────────────────
      console.log('\n【⑦ 注入防护：外部文本不能破坏页面】');
      historyJson = JSON.stringify([{ id: 'x', label: '<img src=x onerror=alert(1)>', state: 'failed', result: '</script><script>alert(1)</script>', details: [], started_at: '', finished_at: '' }]);
      intervals.forEach((fn) => fn());
      setTimeout(() => {
        check('HTML 被转义（&lt;img）', els.opHist.innerHTML.includes('&lt;img'), '未转义');
        check('无未转义的 <script>', !els.opHist.innerHTML.includes('<script>'));

        console.log(`\n${'='.repeat(50)}`);
        console.log(`结果：${pass} 通过 / ${fail} 失败`);
        console.log('='.repeat(50));
        process.exit(fail === 0 ? 0 : 1);
      }, 60);
    }, 60);
  }, 60);
}, 60);
