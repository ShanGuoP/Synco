// 分层纪律断言：把"靠人记的口头约定"变成 grep 级硬约束（与 api-check.js 同风格）。
//
//   node tools/lint-layers.js
//
// 七条规则，全部先在当时现状代码上验绿后才立的（不写跑起来就是红的规矩）：
//   R1  api/*.rs       不摸库          禁 rusqlite（handler 只编排，落库走 repo）
//   R2  repo/*.rs      不碰 HTTP       禁 axum / reqwest
//   R3  service/*.rs   收解包后的参数   禁 axum（"能脱离 HTTP 单测"的承诺兑现成断言）
//   R4  crates/*-core  纯函数红线      禁 tokio / std::fs / rusqlite / axum / reqwest
//                                      只钉 src/——examples/tests 是 lib 的消费者，写对拍
//                                      产物做 IO 天经地义（stitch-core/examples/parity.rs）；
//                                      stitch-core 现在管，photoedit-core M1 落地后自动纳入
//   R5  public/js      fetch 只在 core/api.js
//   R6  已迁前端文件    不许再写中文字面量（账本 DONE_I18N 只增不减）
//   R7  已交钥匙的后端文件 不许再写中文文案（账本与通道判据在 lib/i18n-ledger.js）
//
// 匹配前剥掉行首 // 注释：service/mod.rs 的头注释写着"axum handler 全在 crate::api"，
// 那是契约陈述不是引用，文档里提概念不该报红。只剥行首、不动行中 //（字符串里的 http:// 会误伤）。
'use strict';
const fs = require('fs');
const path = require('path');

const ROOT = path.join(__dirname, '..');
const { codeLines } = require('./lib/code-lines.js');

/* 两本账（已搬完的前端文件、已交钥匙的后端文件）与"什么算界面文案"的通道判据
   都在 lib/i18n-ledger.js：lint 用它当门，i18n-check 用它报进度，出处只能有一个。 */
const { DONE_I18N, DONE_I18N_RS, uiCopy } = require("./lib/i18n-ledger.js");

// pre 的契约是"把要比对的那一行交回来（空串=放过）"，lib 给的是布尔判据，这里包一层
const preLine = (line, raw) => (uiCopy(line, raw) ? line : '');
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

/* 注释怎么剥走 lib/code-lines.js：判据只认代码，注释里提"axum handler 全在 crate::api"
   或 JSDoc 里写 data-i18n 带中文，都不算破规矩。 */


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
    // 通用执行器与取锁已经收成 pub(in crate::repo) / pub(crate)，编译期拦住大半；
    // 这一条补上正则还拦得住的那半：直接握连接、把 SQL 文本或 where 片段递出 DAO。
    id: 'R8', title: 'SQL 只写在 repo（api/ 与 service/ 禁取连接、通用执行器与 SQL 文本）',
    files: () => ['api', 'service'].flatMap(d => walk(path.join(ROOT, 'crates', 'server', 'src', d)).filter(f => f.endsWith('.rs'))),
    bans: [
      ['连接句柄 ctx.db()', /\.db\s*\(\s*\)/],
      ['repo 通用执行器', /\brepo::(one|all|all_on|run|insert_id)\s*\(/],
      ['SQL 文本', /"(SELECT|UPDATE|INSERT|DELETE|PRAGMA|WITH)\s/],
      ['where 片段', /"[A-Za-z_]+=[?]"/],
    ],
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
  {
    // 文案外置的账：只钉"已经搬完的那几个文件"，清单只增不减（i18n-check.js 报还剩多少没搬）。
    // 判据是"引号里带中日韩字符"而不是"这一行有中文"——注释里写中文是天经地义的，那是解释不是文案。
    // 没搬的文件不进清单，是因为它们一进就会红：一条生来就跑不过的规则，人很快就会学会无视它。
    id: 'R6', title: '已迁前端文件不许再写中文字面量（public/js）',
    files: () => DONE_I18N.map(p => path.join(ROOT, p)).filter(f => fs.existsSync(f)),
    bans: [['中文文案', /["'`][^"'`\n]*[\u4e00-\u9fff]/]],
    // 注释（含行尾那句解释）已由 lib/code-lines.js 剥干净，这里只判剩下的代码。
    // 行里带 i18n-keep 的放过：那是发给模型或写进文件的内容，不是界面文案，翻它等于改数据。
    pre: preLine,
  },
  {
    // 后端的账本，与 R6 同一条思路：只钉"已经把文案交给字典的那些文件"。
    // 判据到 #[cfg(test)] 为止——测试里的中文是断言与函数名，不是界面文案，翻它没意义。
    id: 'R7', title: '已交钥匙的后端文件不许再写中文文案（crates/）',
    files: () => DONE_I18N_RS.map(p => path.join(ROOT, p)).filter(f => fs.existsSync(f)),
    bans: [['中文文案', /"[^"\n]*[\u4e00-\u9fff]/]],
    // 控制台/日志/panic 的中文不在账上：那三样写给坐在机器前的人，界面读的是 code，
    // 两条路不会互相冒充。判据在 lib/i18n-ledger.js 的 uiCopy 里，与 i18n-check 报进度同一份。
    pre: preLine,
    // 只算 #[cfg(test)] 之前的行；R6 没有这一条，因为前端没有内联测试模块
    testCut: true,
  },
];

let bad = 0;
for (const rule of RULES) {
  const files = rule.files();
  const hits = [];
  for (const f of files) {
    if (rule.allow && rule.allow(f)) continue;
    let rows = codeLines(f);
    // 后端账本只看测试模块之前：测试里的中文是断言与中文函数名，不是界面文案
    if (rule.testCut) {
      const cut = rows.findIndex(x => /^\s*#\[cfg\(test\)\]/.test(x.raw));
      if (cut >= 0) rows = rows.slice(0, cut);
    }
    for (const { n, raw, line: stripped } of rows) {
      const line = rule.pre ? rule.pre(stripped, raw) : stripped;
      if (!line.trim()) continue;
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
console.log(`\n分层纪律 ${RULES.length} 条全绿。`);
