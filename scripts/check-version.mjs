#!/usr/bin/env node
/**
 * 版本一致性护栏（launcher）
 *
 * 背景：launcher 的版本号手工维护在**三处**，历史上出现过失同步与 tag 空洞
 *       （已打 v0.2.0/v0.2.1/v0.3.1/v0.3.4…v0.3.7，但 **v0.3.0 / v0.3.2 / v0.3.3 从未打过 tag**）。
 *       版本号错一处，自更新比对（`has_newer` 严格大于）即失效 —— 用户永远收不到新版。
 *
 * 本脚本做两件事：
 *   ① 断言三处版本号**逐字相等**：package.json / src-tauri/Cargo.toml / src-tauri/tauri.conf.json
 *   ② 若给了 tag（CI 传 $GITHUB_REF_NAME，本地可传 argv[2]），断言 tag == "v" + 版本号
 *
 * 用法：
 *   node scripts/check-version.mjs              # 只校验三处一致
 *   node scripts/check-version.mjs v0.3.7       # 额外校验 tag 与版本号一致
 *
 * 退出码：0 = 通过；1 = 不一致（CI 里会让构建失败，拦住错误发布）
 */
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');

/** 读取 JSON 里的 version 字段 */
function readJsonVersion(relPath) {
  const abs = join(ROOT, relPath);
  let raw;
  try {
    raw = readFileSync(abs, 'utf8');
  } catch (err) {
    throw new Error(`读不到 ${relPath}：${err.message}`);
  }
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch (err) {
    throw new Error(`${relPath} 不是合法 JSON：${err.message}`);
  }
  if (typeof parsed.version !== 'string' || parsed.version.trim() === '') {
    throw new Error(`${relPath} 里没有非空的 version 字段`);
  }
  return parsed.version.trim();
}

/**
 * 读取 Cargo.toml 里 [package] 段的 version。
 * 只认第一个 [package] 段，避免误取 [dependencies] 里依赖的版本号。
 */
function readCargoVersion(relPath) {
  const abs = join(ROOT, relPath);
  let raw;
  try {
    raw = readFileSync(abs, 'utf8');
  } catch (err) {
    throw new Error(`读不到 ${relPath}：${err.message}`);
  }
  const lines = raw.split(/\r?\n/);
  let inPackage = false;
  for (const line of lines) {
    const section = line.match(/^\s*\[([^\]]+)\]\s*$/);
    if (section) {
      inPackage = section[1].trim() === 'package';
      continue;
    }
    if (!inPackage) continue;
    const m = line.match(/^\s*version\s*=\s*"([^"]+)"\s*$/);
    if (m) return m[1].trim();
  }
  throw new Error(`${relPath} 的 [package] 段里找不到 version`);
}

const SOURCES = [
  { label: 'package.json', read: () => readJsonVersion('package.json') },
  { label: 'src-tauri/Cargo.toml', read: () => readCargoVersion('src-tauri/Cargo.toml') },
  { label: 'src-tauri/tauri.conf.json', read: () => readJsonVersion('src-tauri/tauri.conf.json') },
];

function main() {
  const tag = (process.argv[2] || '').trim();

  console.log('版本一致性护栏 —— launcher');
  console.log('='.repeat(52));

  const results = [];
  const errors = [];

  for (const src of SOURCES) {
    try {
      const v = src.read();
      results.push({ label: src.label, version: v });
      console.log(`  ${src.label.padEnd(28)} = ${v}`);
    } catch (err) {
      errors.push(err.message);
      console.log(`  ${src.label.padEnd(28)} = ❌ ${err.message}`);
    }
  }

  if (errors.length > 0) {
    console.log('\n❌ 读版本号失败：');
    for (const e of errors) console.log(`   - ${e}`);
    process.exit(1);
  }

  // ① 三处必须逐字相等
  const versions = [...new Set(results.map((r) => r.version))];
  if (versions.length !== 1) {
    console.log('\n❌ 三处版本号不一致（发布物版本号错一处，自更新比对即失效）：');
    for (const r of results) console.log(`   - ${r.label} = ${r.version}`);
    console.log('\n   修法：把三处改成同一个版本号。');
    process.exit(1);
  }
  const version = versions[0];
  console.log(`\n✅ 三处版本号一致：${version}`);

  // ② tag（若提供）必须等于 v + 版本号
  if (tag) {
    const expected = `v${version}`;
    if (tag !== expected) {
      console.log(`\n❌ tag 与版本号不一致：`);
      console.log(`   tag          = ${tag}`);
      console.log(`   三处版本号   = ${version}  → 期望 tag = ${expected}`);
      console.log(`\n   修法：要么打正确的 tag（git tag ${expected}），要么先把三处版本号改成 ${tag.replace(/^v/, '')}。`);
      process.exit(1);
    }
    console.log(`✅ tag 与版本号一致：${tag}`);
  } else {
    console.log('ℹ️  未提供 tag 参数，跳过 tag 校验（CI 会传 $GITHUB_REF_NAME）。');
  }

  console.log('\n结论：版本号护栏通过 ✅');
}

main();
