// Rust 版接口自检：起一个隔离实例（临时 DATA + 随机端口），先扫一遍所有端点确认没有 5xx，
// 再跑 M3 图像服务化 / 服务端队列那 19 条行为断言。不碰你正在跑的 7861，也不写真实 data/。
//
//   node tools/api-check.js          # 跑全部
//   node tools/api-check.js --keep   # 保留临时 DATA 便于复查
//
// 先 cargo build -p synco-server（要 target/debug/synco.exe）。
'use strict';
const { spawn } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');
const assert = require('assert');

const ROOT = path.join(__dirname, '..');
const BIN = path.join(ROOT, 'target', 'debug', 'synco.exe');

function tmpData() {
  const d = path.join(os.tmpdir(), `synco-check-${Date.now()}`);
  fs.mkdirSync(d, { recursive: true });
  return d;
}

async function boot(dataDir) {
  try { fs.unlinkSync(path.join(dataDir, 'port.txt')); } catch {}
  const proc = spawn(BIN, [], { cwd: ROOT, env: { ...process.env, SYNCO_DATA: dataDir, SYNCO_PORT: '0' }, stdio: ['ignore', 'pipe', 'pipe'] });
  const log = [];
  proc.stdout.on('data', b => log.push(b.toString()));
  proc.stderr.on('data', b => log.push(b.toString()));
  const t0 = Date.now();
  let port = 0;
  while (!port) {
    try { const p = fs.readFileSync(path.join(dataDir, 'port.txt'), 'utf8').trim(); if (/^\d+$/.test(p)) port = Number(p); } catch {}
    if (proc.exitCode !== null) throw new Error(`服务提前退出 code=${proc.exitCode}：\n${log.join('')}`);
    if (Date.now() - t0 > 20000) throw new Error(`等 port.txt 超时：\n${log.join('')}`);
    await new Promise(r => setTimeout(r, 120));
  }
  const base = `http://127.0.0.1:${port}`;
  // 端口写进去了不等于服务还活着：axum 建路由时 panic 就是"起了又死"，先探一口
  const t1 = Date.now();
  while (Date.now() - t1 < 8000) {
    try { const r = await fetch(base + '/api/projects', { signal: AbortSignal.timeout(1500) }); if (r.ok) return { proc, base, log }; } catch {}
    if (proc.exitCode !== null) throw new Error(`服务起来了又退了（code=${proc.exitCode}）：\n${log.join('')}`);
    await new Promise(r => setTimeout(r, 150));
  }
  throw new Error(`服务在 ${base} 上没答话：\n${log.join('')}`);
}

async function req(base, method, p, body, extra = {}) {
  const opt = { method, headers: { ...extra }, signal: AbortSignal.timeout(8000) };
  if (body !== undefined) {
    opt.body = JSON.stringify(body);
    opt.headers['content-type'] = 'application/json';
  }
  const r = await fetch(base + p, opt);
  const ct = r.headers.get('content-type') || '';
  let val = null;
  if (ct.includes('json')) { try { val = await r.json(); } catch { val = '<坏 JSON>'; } }
  else { const buf = Buffer.from(await r.arrayBuffer()); val = { __bytes: buf.length, __head: buf.subarray(0, 16).toString('hex') }; }
  return { status: r.status, ct, cache: r.headers.get('cache-control'), etag: r.headers.get('etag'), body: val };
}

// 1x1 PNG：只测流程与响应形状，像素级对拍在 stitch-core 的回归里
const PNG = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==';

// 按顺序跑：建项目/建图是后面一堆用例的前置
const SWEEP = [
  ['项目列表初始为空', 'GET', '/api/projects'],
  ['建项目', 'POST', '/api/projects', { name: '中文 项目 名', files: [{ name: 'a#1 b%.png', b64: PNG, w: 1, h: 1 }] }],
  ['项目详情', 'GET', '/api/projects/1'],
  ['项目详情 404', 'GET', '/api/projects/999'],
  ['存遮罩', 'POST', '/api/images/1/mask', { b64: PNG }],
  ['单图', 'GET', '/api/images/1'],
  ['单图 404', 'GET', '/api/images/999'],
  ['清遮罩', 'POST', '/api/images/1/mask', {}],
  ['项目设置读写', 'POST', '/api/projects/1/settings', { loras: [{ name: 'x', strength: 1, enabled: true }] }],
  ['参数 cfg', 'GET', '/api/cfg'],
  ['工坊设置', 'GET', '/api/settings'],
  ['工作流路径写入', 'POST', '/api/settings/workflow', { path: 'D:/不存在的目录/wf.json' }],
  ['云端保存', 'POST', '/api/cloud', { kind: 'cloud', base: 'http://127.0.0.1:1/v1', model: 'm-check', key: 'sk-local-check', timeout: '5000', concurrency: 9, stitch_expand: 64, stitch_feather: 200, stitch_edge: 1024 }],
  ['云端读回', 'GET', '/api/cloud'],
  ['云端 edit 缺图', 'POST', '/api/cloud/edit', { image_id: 999, mask_b64: PNG, settings: {} }],
  ['预设 建', 'POST', '/api/presets', { name: '默认预设', prompt: 'p', negative: 'n', steps: 25, cfg: 3, loras: [{ name: 'L', strength: 0.8 }] }],
  ['预设 重名', 'POST', '/api/presets', { name: '默认预设' }],
  ['预设 无名字', 'POST', '/api/presets', { name: '   ' }],
  ['预设 列表', 'GET', '/api/presets'],
  ['预设 更新', 'POST', '/api/presets/1/update', { name: '改名', steps: 30, scope: 'global' }],
  ['预设 删除', 'POST', '/api/presets/1/delete'],
  ['后端列表', 'GET', '/api/backends'],
  ['后端 登记', 'POST', '/api/backends', { url: '127.0.0.1:9999', label: '冒烟' }],
  ['后端 移除', 'POST', '/api/backends/remove', { url: 'http://127.0.0.1:9999' }],
  ['后端 非法地址', 'POST', '/api/backends', { url: 'ftp://x' }],
  ['导出 未设置', 'GET', '/api/export'],
  ['导出 相对路径拒绝', 'POST', '/api/export/dir', { dir: 'relative/out' }],
  ['导出 缺 ids', 'POST', '/api/export/run', { result_ids: [] }],
  ['体检', 'GET', '/api/setup'],
  ['体检进度', 'GET', '/api/setup/progress'],
  ['设根目录', 'POST', '/api/setup/root', { path: 'D:/不存在的便携包' }],
  ['提交无遮罩', 'POST', '/api/run', { image_ids: [1], settings: { prompt: 'x', steps: 20, cfg: 3 } }],
  ['提交不存在的图', 'POST', '/api/run', { image_ids: [999], settings: {} }],
  ['结果 404', 'GET', '/api/results/999'],
  ['中断 404', 'POST', '/api/results/999/interrupt'],
  ['删结果 404', 'DELETE', '/api/results/999'],
  ['fork 404', 'POST', '/api/results/999/fork'],
  ['文件白名单', 'GET', '/file/app.db'],
  ['静态首页', 'GET', '/'],
  ['静态脚本', 'GET', '/public/js/app.js'],
  ['静态字体', 'GET', '/public/fonts/NotoSerifSC-VF.ttf'],
  ['未知路由', 'GET', '/api/nonsense'],
  ['未知页面', 'GET', '/nope'],
];

// 这些端点按设计就要回 5xx（上游真的连不上），不算回归
const EXPECTED_5XX = new Set(['云端探活（无网络也要有结构）']);

// 路径穿越回归。判据分两档：
//   403 = 这一形态能原样送到服务端（被 rel_ok 逐段挡下）
//   0   = 客户端的 URL 归一先把 .. 段吃掉了，服务端只会看到一条普通路径 → 只要求拿不到 200
// 原样送出去的裸 `..` / `%5c` 形态由 Rust 侧的 util::rel_ok_逐段查_连反斜杠一起 钉住。
const TRAVERSAL = [
  ['/public/js%5c..%5c..%5c..%5c..%5cWindows%5cwin.ini', 403, '%5c 反斜杠逐段上跳'],
  ['/public/%2e%2e/%2e%2e/%2e%2e/%2e%2e/Windows/win.ini', 0, '%2e%2e（undici 会归一掉）'],
  ['/public/..%5cdata%5capp.db', 403, '直接摸 app.db'],
  ['/file/projects%5c..%5c..%5c..%5cWindows%5cwin.ini', 403, '/file/ 同一条洞'],
  ['/public/js/../../../../Windows/win.ini', 0, '裸 ..（会被 URL 归一）'],
];
async function traversalCheck(base) {
  const fails = [];
  for (const [url, want, label] of TRAVERSAL) {
    const r = await fetch(base + url, { signal: AbortSignal.timeout(5000) });
    const bad = want === 0 ? r.status === 200 : r.status !== want;
    console.log(`${bad ? '  ✗' : '  ✓'} ${label} → ${r.status}`);
    if (bad) fails.push(label);
  }
  return fails;
}

// ---- M3 服务端图像化 / 队列的行为自检 ------------------------------------------
// 云端地址一律指到 127.0.0.1:1（本机一定拒接），既不打扰真实服务，也不动你的 key
async function m3SelfCheck(base, dataDir) {
  const fails = [];
  const ok = (name, cond, detail) => {
    console.log(`${cond ? '  ✓' : '  ✗'} ${name}${cond ? '' : '：' + detail}`);
    if (!cond) fails.push(name);
  };
  await req(base, 'POST', '/api/cloud', { kind: 'cloud', base: 'http://127.0.0.1:1/v1', key: 'sk-local-check', model: 'm-check', timeout: '30000' });
  const p = await req(base, 'POST', '/api/projects', { name: 'M3 自检', files: [{ name: 'a.png', b64: PNG, w: 3, h: 2 }] });
  const iid = p.body.image_ids[0];
  await req(base, 'POST', `/api/images/${iid}/mask`, { b64: PNG });

  let img = {};
  for (let i = 0; i < 24 && !img.thumb_url; i++) {
    img = (await req(base, 'GET', `/api/images/${iid}`)).body;
    if (!img.thumb_url) await new Promise((r) => setTimeout(r, 150));
  }
  ok('导入后后台补出 320 档', /^\/file\/.+_thumb\.jpg$/.test(String(img.thumb_url)), JSON.stringify(img.thumb_url));
  ok('没超出档位就不切 proxy', img.proxy_url === null, JSON.stringify(img.proxy_url));
  const th = await req(base, 'GET', `/api/images/${iid}/thumb`);
  ok('thumb 端点回同一个档', th.status === 200 && th.body.thumb_url === img.thumb_url, JSON.stringify(th.body));
  const tf = await req(base, 'GET', img.thumb_url);
  ok('派生档按 immutable 发', tf.status === 200 && /immutable/.test(tf.cache || ''), `${tf.status} / ${tf.cache}`);
  const tiles = await req(base, 'GET', `/api/images/${iid}/tiles`);
  ok('小图瓦片退回整图而不是一堆 1×1', tiles.status === 200 && tiles.body.tile === 0 && tiles.body.levels.length === 0, JSON.stringify(tiles.body).slice(0, 160));
  const tiles2 = await req(base, 'GET', `/api/images/${iid}/tiles`);
  ok('瓦片清单第二次读命中缓存', tiles2.status === 200 && JSON.stringify(tiles2.body) === JSON.stringify(tiles.body), JSON.stringify(tiles2.body).slice(0, 160));

  const mk = await req(base, 'GET', img.mask_url);
  ok('蒙版是 must-revalidate 并带 ETag', mk.status === 200 && /no-cache/.test(mk.cache || '') && !!mk.etag, `${mk.status} / ${mk.cache} / ${mk.etag}`);
  const mk2 = await req(base, 'GET', img.mask_url, undefined, { 'if-none-match': mk.etag });
  ok('蒙版条件命中回 304', mk2.status === 304, `${mk2.status}`);

  const bad1 = await req(base, 'POST', '/api/cloud/queue', { image_ids: [], settings: { prompt: 'x' } });
  ok('队列拒绝空列表', bad1.status === 400, JSON.stringify(bad1.body));
  const bad2 = await req(base, 'POST', '/api/cloud/queue', { image_ids: [iid], settings: { prompt: '   ' } });
  ok('队列拒绝空提示词', bad2.status === 400, JSON.stringify(bad2.body));
  const q = await req(base, 'POST', '/api/cloud/queue', { image_ids: [iid], settings: { prompt: '跑一遍状态机' } });
  const rid = (q.body.results || [])[0]?.result_id;
  ok('批量提交建行并入队', q.status === 200 && !!rid && q.body.queued === 1, JSON.stringify(q.body));
  let row = {};
  for (let i = 0; i < 40; i++) {
    row = (await req(base, 'GET', `/api/results/${rid}`)).body;
    if (row.status === 'error' || row.status === 'done') break;
    await new Promise((r) => setTimeout(r, 150));
  }
  // 这张自检图是全不透明的（蒙版语义里"透明才重绘"），所以到不了云端调用就被判 NoInk；
  // 要验的是 queued→running→终态 这台状态机自己走完，不是云端应答
  ok('queued→running→终态 走通', row.status === 'error' || row.status === 'done', JSON.stringify(row).slice(0, 200));
  console.log(`      落定原因：${String(row.error || '（成功出图）').slice(0, 90)}`);
  const snap = await req(base, 'GET', '/api/cloud/queue');
  ok('队列快照有结构', snap.status === 200 && Array.isArray(snap.body.items) && typeof snap.body.concurrency === 'number', JSON.stringify(snap.body).slice(0, 200));
  const adopt = await req(base, 'POST', '/api/cloud/adopt', { result_id: rid, image_b64: PNG });
  ok('adopt 端点已退役', adopt.status === 404, `${adopt.status} ${JSON.stringify(adopt.body)}`);
  const edge = await req(base, 'POST', '/api/settings/proxy-edge', { proxy_edge: 2048 });
  ok('档位旋钮收夹紧并回显', edge.status === 200 && edge.body.proxy_edge === 2048, JSON.stringify(edge.body));
  const edge2 = await req(base, 'POST', '/api/settings/proxy-edge', { proxy_edge: 99 });
  ok('档位越界被夹回下限', edge2.body.proxy_edge === 1024, JSON.stringify(edge2.body));
  const st = await req(base, 'GET', '/api/settings');
  ok('设置接口回显当前档位', st.body.proxy_edge === 1024, JSON.stringify(st.body).slice(0, 160));

  // 派生档要跟着图一起删掉，不然目录只涨不落
  await req(base, 'DELETE', `/api/images/${iid}`);
  const pidDir = path.join(dataDir, 'projects', String(p.body.id));
  const filesLeft = fs.existsSync(pidDir) ? fs.readdirSync(pidDir) : ['目录已删'];
  ok('删图连带清掉派生档', filesLeft.length === 0 || filesLeft[0] === '目录已删', JSON.stringify(filesLeft));
  return fails;
}

async function main() {
  assert(fs.existsSync(BIN), `先 cargo build -p synco-server（找不到 ${BIN}）`);
  const dataDir = tmpData();
  const { proc, base, log } = await boot(dataDir);
  console.log(`Rust ${base.replace(/^http:\/\/127\.0\.0\.1:/, '')} @ ${dataDir}\n`);

  let bad = 0;
  for (const [title, method, url, body] of SWEEP) {
    let r;
    try { r = await req(base, method, url, body); } catch (e) { bad++; console.log(`✗ ${title}  抛了 ${e.message}`); continue; }
    const boom = r.status >= 500 && !EXPECTED_5XX.has(title);
    if (boom) bad++;
    console.log(`${boom ? '✗' : '✓'} ${title}  (${r.status})${boom ? ' ' + JSON.stringify(r.body).slice(0, 160) : ''}`);
  }

  console.log('\n路径穿越（回归：这几条曾经能读到 app.db）');
  const tv = await traversalCheck(base);

  console.log('\nM3 服务端图像化 / 队列自检');
  const m3 = await m3SelfCheck(base, dataDir);
  proc.kill();
  if (bad || m3.length || tv.length) console.log('\n—— 服务输出 ——\n' + log.join('').split('\n').slice(-40).join('\n'));
  if (!process.argv.includes('--keep')) {
    // 进程还在退的时候 Windows 会锁着目录，等一会儿再删，删不掉也不算失败
    await new Promise(r => setTimeout(r, 600));
    try { fs.rmSync(dataDir, { recursive: true, force: true }); } catch { console.log(`（临时目录留着了：${dataDir}）`); }
  }
  console.log(`\n端点扫查失败 ${bad} 个；穿越回归失败 ${tv.length} 条；M3 自检失败 ${m3.length} 项`);
  process.exit(bad || tv.length || m3.length ? 1 : 0);
}

main().catch(e => { console.error(e); process.exit(2); });
