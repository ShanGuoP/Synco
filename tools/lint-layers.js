// 分层纪律断言：把"靠人记的口头约定"变成 grep 级硬约束（与 api-check.js 同风格）。
//
//   node tools/lint-layers.js
//
// 五条规则，全部先在 0.2.1 现状代码上验绿后才立的（不写跑起来就是红的规矩）：
//   R1  api/*.rs       不摸库          禁 rusqlite（handler 只编排，落库走 repo）
//   R2  repo/*.rs      不碰 HTTP       禁 axum / reqwest
//   R3  service/*.rs   收解包后的参数   禁 axum（"能脱离 HTTP 单测"的承诺兑现成断言）
//   R4  crates/*-core  纯函数红线      禁 tokio / std::fs / rusqlite / axum / reqwest
//                                      只钉 src/——examples/tests 是 lib 的消费者，写对拍
//                                      产物做 IO 天经地义（stitch-core/examples/parity.rs）；
//                                      stitch-core 现在管，photoedit-core M1 落地后自动纳入
//   R5  public/js      fetch 只在 core/api.js
//
// 匹配前剥掉行首 // 注释：service/mod.rs 的头注释写着"axum handler 全在 crate::api"，
// 那是契约陈述不是引用，文档里提概念不该报红。只剥行首、不动行中 //（字符串里的 http:// 会误伤）。
'use strict';
const fs = require('fs');
const path = require('path');

const ROOT = path.join(__dirname, '..');
const rel = p => path.relative(ROOT, p).replace(/\\/g, '/');

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

function codeLines(file) {
  const src = fs.readFileSync(file, 'utf8');
  return src.split(/\r?\n/)
    .map((line, i) => ({ n: i + 1, line: /^\s*\/\//.test(line) ? '' : line }))
    .filter(x => x.line.trim());
}

// crates/ 下所有 *-core 目录：R4 的对象。目录名模式匹配而不是写死 stitch-core，
// photoedit-core（M1）以及以后任何 *-core 都自动纳管。
function coreDirs() {
  try {
    return fs.readdirSync(path.join(ROOT, 'crates'), { withFileTypes: true })
      .filter(e => e.isDirectory() && /-core$/.test(e.name)).map(e => e.name);
  } catch { return []; }
}

// 每条规则：files() 收集待扫文件，bans = [标签, 正则]，命中即违规；allow() 豁免个别文件。
const RULES = [
  {
    id: 'R1', title: 'api 不摸库（api/*.rs 禁 rusqlite）',
    files: () => walk(path.join(ROOT, 'crates', 'server', 'src', 'api')).filter(f => f.endsWith('.rs')),
    bans: [['rusqlite', /\brusqlite\b/]],
  },
  {
    id: 'R2', title: 'repo 不碰 HTTP（repo/*.rs 禁 axum / reqwest）',
    files: () => walk(path.join(ROOT, 'crates', 'server', 'src', 'repo')).filter(f => f.endsWith('.rs')),
    bans: [['axum', /\baxum\b/], ['reqwest', /\breqwest\b/]],
  },
  {
    id: 'R3', title: 'service 收解包后的参数（service/*.rs 禁 axum）',
    files: () => walk(path.join(ROOT, 'crates', 'server', 'src', 'service')).filter(f => f.endsWith('.rs')),
    bans: [['axum', /\baxum\b/]],
  },
  {
    id: 'R4', title: '*-core 纯函数红线（禁 tokio / std::fs / rusqlite / axum / reqwest）',
    files: () => coreDirs().flatMap(d => walk(path.join(ROOT, 'crates', d, 'src')).filter(f => f.endsWith('.rs'))),
    bans: [['tokio', /\btokio\b/], ['std::fs', /\bstd::fs\b/], ['rusqlite', /\brusqlite\b/], ['axum', /\baxum\b/], ['reqwest', /\breqwest\b/]],
  },
  {
    id: 'R5', title: 'fetch 只在 core/api.js（public/js/**.js）',
    files: () => walk(path.join(ROOT, 'public', 'js')).filter(f => f.endsWith('.js')),
    bans: [['fetch(', /\bfetch\s*\(/]],
    allow: f => rel(f) === 'public/js/core/api.js',
  },
];

let bad = 0;
for (const rule of RULES) {
  const files = rule.files();
  const hits = [];
  for (const f of files) {
    if (rule.allow && rule.allow(f)) continue;
    for (const { n, line } of codeLines(f)) {
      for (const [tag, re] of rule.bans) {
        if (re.test(line)) hits.push(`      ${rel(f)}:${n}  [${tag}]  ${line.trim().slice(0, 110)}`);
      }
    }
  }
  if (hits.length) bad++;
  console.log(`${hits.length ? '✗' : '✓'} ${rule.id} ${rule.title}（扫 ${files.length} 个文件）`);
  for (const h of hits) console.log(h);
  if (!files.length) console.log('      ⚠ 一个文件都没扫到——目录是不是改名了？');
}

if (!fs.existsSync(path.join(ROOT, 'crates', 'photoedit-core'))) {
  console.log('（photoedit-core 未建：R4 现只看 stitch-core，M1 落地后自动纳入）');
}

if (bad) {
  console.log(`\n分层纪律破了 ${bad} 条规则——先改代码，别改规则。`);
  process.exit(1);
}
console.log('\n分层纪律 5 条全绿。');
