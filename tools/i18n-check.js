// 字典的体检：中英键位对齐、前端还有多少中文没搬、字典里有没有没人用的键。
//
//   node tools/i18n-check.js          # 只报，不改
//   node tools/i18n-check.js --strict # 把"警告"也算失败（搬完之后用它收尾）
//
// 为什么单独一个工具而不是塞进 lint-layers：lint 那六条是"结构不许破"，
// 这条是"进度看得见"——它要报数、要排队，破口在哪还剩多少，搬的人每天要看。
//
// 判"中文文案"用引号紧邻中文，注释里的中文不算（那是解释，不是界面文字）。
'use strict';
const fs = require('fs');
const path = require('path');

const ROOT = path.join(__dirname, '..');
const { codeLines } = require('./lib/code-lines.js');
const { DONE_I18N_RS, uiCopy, isConsoleOnly } = require('./lib/i18n-ledger.js');
const rel = p => path.relative(ROOT, p).replace(/\\/g, '/');
const CJK_IN_STRING = /["'`][^"'`\n]*[\u4e00-\u9fff]/;

function walk(dir, out = []) {
  let entries;
  try { entries = fs.readdirSync(dir, { withFileTypes: true }); } catch { return out; }
  for (const e of entries) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p, out);
    else if (e.isFile()) out.push(p);
  }
  return out;
}

/* 注释与字符串怎么区分走 lib/code-lines.js——那里写着为什么不能只看 /* 的字面出现 */

function flat(obj, prefix = '', out = []) {
  for (const [k, v] of Object.entries(obj || {})) {
    const key = prefix ? `${prefix}.${k}` : k;
    if (v && typeof v === 'object' && !Array.isArray(v)) flat(v, key, out);
    else out.push(key);
  }
  return out;
}

const strict = process.argv.includes('--strict');
const zhPath = path.join(ROOT, 'public', 'locales', 'zh.json');
const enPath = path.join(ROOT, 'public', 'locales', 'en.json');
let fails = 0;
const bad = m => { fails++; console.log('  ✗ ' + m); };
const warn = m => console.log((fails ? '  · ' : '  ⚠ ') + m);

const zh = JSON.parse(fs.readFileSync(zhPath, 'utf8'));
const en = JSON.parse(fs.readFileSync(enPath, 'utf8'));
const kz = flat(zh), ke = flat(en);
const onlyZh = kz.filter(k => !ke.includes(k));
const onlyEn = ke.filter(k => !kz.includes(k));

console.log(`\n字典：zh ${kz.length} 条 · en ${ke.length} 条`);
if (onlyZh.length) bad(`en 缺 ${onlyZh.length} 条：${onlyZh.slice(0, 8).join(', ')}${onlyZh.length > 8 ? ' …' : ''}`);
if (onlyEn.length) bad(`zh 缺 ${onlyEn.length} 条：${onlyEn.slice(0, 8).join(', ')}${onlyEn.length > 8 ? ' …' : ''}`);
if (!onlyZh.length && !onlyEn.length) console.log('  ✓ 中英键位一一对应');

// 引用扫描：源码里任何被引号包住的完整键名都算引用（t('a.b')、data-i18n="a.b"、
// 数组里的 ['zh','a.b'] 都算）；模板拼出来的 `nav.${k}` 那种只能按前缀认。
// Rust 也算引用方：服务端交出去的 srv.* 钥匙就写在那边的源码里。
const sources = walk(path.join(ROOT, 'public', 'js')).filter(f => f.endsWith('.js'));
const html = path.join(ROOT, 'public', 'index.html');
const rsFiles = [];
const walkRs = dir => {
  let es;
  try { es = fs.readdirSync(dir, { withFileTypes: true }); } catch { return; }
  for (const e of es) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walkRs(p);
    else if (e.name.endsWith('.rs')) rsFiles.push(p);
  }
};
walkRs(path.join(ROOT, 'crates'));
// 后端只算代码行：文档注释里举的例子（`bad_args("srv.adjust.clamped", …)`）不是真引用
const rsCode = rsFiles.map(f => codeLines(f).map(x => x.line).join('\n')).join('\n');
const hay = sources.map(f => fs.readFileSync(f, 'utf8')).join('\n') + '\n' + fs.readFileSync(html, 'utf8') + '\n' + rsCode;
const quoted = new Set((hay.match(/["'`][\w.]+["'`]/g) || []).map(s => s.slice(1, -1)));
const dynPrefixes = (hay.match(/["'`][\w.]*\.\$\{/g) || []).map(m => m.slice(1, -3) + '.');
const unused = kz.filter(k => !quoted.has(k) && !dynPrefixes.some(p => k.startsWith(p)));
if (unused.length) warn(`${unused.length} 条键在代码里找不到直接引用（可能是动态键，也可能真没人用）：${unused.slice(0, 8).join(', ')}`);
else console.log('  ✓ 字典里没有悬空键');

// 后端交出去的钥匙：Rust 源码里出现的 "srv.*" 必须两份字典都有，否则界面只能看到裸钥匙
const rsCodes = new Set();
for (const m of rsCode.matchAll(/"(srv\.[A-Za-z0-9_.]+)"/g)) rsCodes.add(m[1]);
const zhSet = new Set(kz), enSet = new Set(ke);
const rsMissing = [...rsCodes].filter(k => !zhSet.has(k) || !enSet.has(k));
if (rsMissing.length) bad(`${rsMissing.length} 把 Rust 用到的钥匙不在字典里（界面会看到裸钥匙名）：${rsMissing.slice(0, 6).join(', ')}`);
else console.log(`  ✓ Rust 用到的 ${rsCodes.size} 把 srv.* 钥匙都在字典里`);

// 引用检查：搬过的文件必须真的 import 了取文案的入口。
// 批量把 '中文' 换成 t('a.b') 时最容易漏的就是这一行——语法、R6、键位对齐全都照样绿，
// 但一打开那个分区就 ReferenceError，界面空一块。浏览器探针抓到过两次（backends.js / phrases.js）。
const noImport = [];
for (const f of sources) {
  const src = fs.readFileSync(f, 'utf8');
  const body = codeLines(f).map(x => x.line).join('\n');
  const tCalled = /(^|[^.\w$])t\(/.test(body);
  const trCalled = /(^|[^.\w$])tr\(/.test(body);
  if (!tCalled && !trCalled) continue;
  const imp = /import\s*\{([^}]*)\}\s*from\s*['"][^'"]*i18n\.js['"]/.exec(src);
  const own = /export\s+(?:async\s+)?function\s+t\(|export\s+const\s+t\s*=/.test(src);
  const names = imp ? imp[1].split(',').map(s => s.trim().split(/\s+as\s+/).pop()).filter(Boolean) : [];
  const miss = [];
  if (tCalled && !own && !names.includes('t') && !names.includes('tr')) miss.push('用了 t() 却没 import t');
  if (trCalled && !own && !names.includes('tr')) miss.push('用了 tr() 却没 import { t as tr }');
  if (miss.length) noImport.push(`${rel(f)}  [${miss.join('；')}]`);
}
if (noImport.length) bad(`${noImport.length} 个文件调了 t()/tr() 却没引进对应入口：\n     ${noImport.join('\n     ')}`);
else console.log('  ✓ t()/tr() 的引用都拿到了入口');

// 进度：还剩多少中文字面量没搬
const counts = [];
for (const f of [...sources, html]) {
  const isHtml = f === html;
  // i18n-keep 那行照 R6 的口径放过：那是发给模型或写进文件的内容，不是界面文案
  // HTML 里带 data-i18n 的行也放过：那份字面量是首帧之前的兜底显示，applyDom 起来就按字典换掉
  const hits = codeLines(f, { html: isHtml })
    .filter(x => !/i18n-keep/.test(x.raw) && !(isHtml && /data-i18n/.test(x.raw)) && CJK_IN_STRING.test(x.line));
  if (hits.length) counts.push({ file: rel(f), n: hits.length, first: hits[0].n });
}
const done = counts.filter(c => c.file.startsWith('public/js/core/i18n') || c.file === 'public/js/shell.js');
if (done.length) bad('账本里标了"已搬完"的文件仍有中文字面量：' + done.map(d => `${d.file}(${d.n})`).join(' '));
counts.sort((a, b) => b.n - a.n);
const total = counts.reduce((s, c) => s + c.n, 0);
console.log(`\n待搬：${counts.length} 个文件 · 约 ${total} 行中文字面量`);
for (const c of counts) console.log(`  ${String(c.n).padStart(4)} 行  ${c.file}  (首处 :${c.first})`);
if (!counts.length) console.log('  ✓ 前端界面文案已全部外置');

/* 后端账（I4）：Rust 里直接进界面的中文。只报数、不判失败——这一格的收法与前端不同：
   服务端要改成 {code, args} 让字典当唯一文案来源，动一条就得同步改 harness 里那条中文断言，
   所以进度要看得见，但一条跑得红的规矩只会让人学会无视它。
   判据与 R7 同一份（lib/i18n-ledger.js）：注释、日志/panic 通道、i18n-keep 那种磁盘内容都不算，
   控制台程序（src/bin/、examples/）整份按通道排除。#[cfg(test)] 之后也不算——那是断言。 */
const rsRows = [];
for (const f of rsFiles) {
  if (isConsoleOnly(rel(f))) continue;
  const src = fs.readFileSync(f, 'utf8');
  const cut = src.indexOf('#[cfg(test)]');
  const before = new Set((cut < 0 ? src : src.slice(0, cut)).split(/\r?\n/).map((_, i) => i + 1));
  const hits = codeLines(f).filter(x => before.has(x.n) && uiCopy(x.line, x.raw) && /"[^"\n]*[\u4e00-\u9fff]/.test(x.line));
  if (hits.length) rsRows.push({ file: rel(f), n: hits.length, first: hits[0].n });
}
rsRows.sort((a, b) => b.n - a.n);
const rsTotal = rsRows.reduce((s, r) => s + r.n, 0);
console.log(`\n后端待搬（界面文案那一路，crates/）：${rsRows.length} 个文件 · ${rsTotal} 行中文字面量`);
for (const r of rsRows) console.log(`  ${String(r.n).padStart(4)} 行  ${r.file}  (首处 :${r.first})`);
if (!rsRows.length) console.log(`  ✓ 后端界面文案已全部交给字典（账本 ${DONE_I18N_RS.length} 个文件；日志与运维菜单按通道另算）`);

if (fails) {
  console.log(`\n字典体检失败 ${fails} 项。`);
  process.exit(1);
}
console.log(strict && total ? `\n--strict：还有 ${total} 行没搬完。` : '\n字典体检通过。');
if (strict && total) process.exit(1);
