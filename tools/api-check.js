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
  const d = path.resolve(os.tmpdir(), `synco-check-${Date.now()}`);
  /* 隔离只靠这一个目录名：TMPDIR 被人指到仓库里时，自检写的就是他真实的 data/（几百 MB 的原片与
     明文 key）。起手断言一次，比在注释里提醒"别忘了 SYNCO_DATA"可靠。 */
  const root = path.resolve(ROOT);
  if (d === root || d.startsWith(root + path.sep)) throw new Error(`临时 DATA 落在仓库里了：${d}（TMPDIR=${os.tmpdir()}）`);
  fs.mkdirSync(d, { recursive: true });
  return d;
}

async function boot(dataDir) {
  const proc = spawn(BIN, [], { cwd: ROOT, env: { ...process.env, SYNCO_DATA: dataDir, SYNCO_PORT: '0' }, stdio: ['ignore', 'pipe', 'pipe'] });
  const log = [];
  proc.stdout.on('data', b => log.push(b.toString()));
  proc.stderr.on('data', b => log.push(b.toString()));
  // 端口不再往盘上落一份：服务起来会打 `SYNCO_URL=http://127.0.0.1:<port>`，从输出里读就是它自己绑成的那个
  const portOf = () => { const m = /SYNCO_URL=http:\/\/127\.0\.0\.1:(\d+)/.exec(log.join('')); return m ? Number(m[1]) : 0; };
  const t0 = Date.now();
  let port = 0;
  // 数据目录已经被人占着（桌面壳开着）时，服务不重试随机端口，直接带原因退出——
  // 这条从 boot 里冒出来是"已经有另一个在用了"，不是"起不来"
  const held = () => /已经有另一个 Synco/.test(log.join(''));
  while (!port) {
    port = portOf();
    if (port) break;
    if (proc.exitCode !== null && held()) throw new Error('这个数据目录已经有另一个 Synco 在用了（自检要的是隔离实例：先设 SYNCO_DATA）');
    if (proc.exitCode !== null) throw new Error(`服务提前退出 code=${proc.exitCode}：\n${log.join('')}`);
    if (Date.now() - t0 > 20000) throw new Error(`等 SYNCO_URL 超时：\n${log.join('')}`);
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
  let text = null;
  if (ct.includes('json')) { try { val = await r.json(); } catch { val = '<坏 JSON>'; } }
  else if (ct.includes('html')) { text = await r.text(); val = { __bytes: Buffer.byteLength(text) }; }
  else { const buf = Buffer.from(await r.arrayBuffer()); val = { __bytes: buf.length, __head: buf.subarray(0, 16).toString('hex') }; }
  return {
    status: r.status, ct, cache: r.headers.get('cache-control'), etag: r.headers.get('etag'),
    csp: r.headers.get('content-security-policy'), text, body: val, __base: base,
  };
}

// 1x1 PNG：只测流程与响应形状，像素级对拍在 stitch-core 的回归里
const PNG = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==';

// 按顺序跑：建项目/建图是后面一堆用例的前置。
// 每行是 [名字, 方法, 路径, body, 期望状态码, 附加断言, 额外请求头]。
// 状态码必须逐条写死：以前这 49 条只判"没到 5xx"，白名单那条真把 app.db 端出去也是绿的，
// 而两条 403（POST 不带 body 被守卫拦在鉴权前）里有一条根本没进过 handler。
const SWEEP = [
  ['项目列表初始为空', 'GET', '/api/projects', undefined, [200], r => (Array.isArray(r.body) && r.body.length === 0 ? '' : '不是空列表')],
  ['建项目', 'POST', '/api/projects', { name: '中文 项目 名', files: [{ name: 'a#1 b%.png', b64: PNG, w: 1, h: 1 }] }, [200],
    r => (r.body?.id > 0 && r.body?.image_ids?.length === 1 ? '' : `没建成：${JSON.stringify(r.body).slice(0, 80)}`)],
  ['项目详情', 'GET', '/api/projects/1', undefined, [200]],
  ['项目详情 404', 'GET', '/api/projects/999', undefined, [404]],
  ['存遮罩', 'POST', '/api/images/1/mask', { b64: PNG }, [200]],
  ['单图', 'GET', '/api/images/1', undefined, [200], r => (r.body?.id === 1 ? '' : '回的不是这张')],
  ['单图 404', 'GET', '/api/images/999', undefined, [404]],
  ['清遮罩', 'POST', '/api/images/1/mask', {}, [200]],
  ['派生列表（没有子图也回 200）', 'GET', '/api/images/1/derived', undefined, [200], r => (Array.isArray(r.body?.images) ? '' : '没有 images 数组')],
  /* ---------------- 0.3 本地调整：形状、域与"参数不是覆盖"这几件事 ----------------
     像素级的正确性与真实渲染由 tools/adjust-check.js 在隔离实例上跑一张 1600×1200（长边超出 proxy 档）的图去验，
     这一批只钉契约：状态码、字段形状、越界夹逼、坏输入被拒——回归里最容易被改坏的就是这些。 */
  ['读调整参数（没存过=全默认）', 'GET', '/api/images/1/adjust', undefined, [200],
    r => (r.body?.ops?.v === 1 && r.body?.ops?.color?.exposure === 0 && Array.isArray(r.body?.presets) && r.body.presets.length >= 8 && Array.isArray(r.body?.luts) ? '' : `形状不对：${JSON.stringify(r.body).slice(0, 120)}`)],
  ['调整参数 404', 'GET', '/api/images/999/adjust', undefined, [404]],
  ['调整参数越界被夹并记账', 'POST', '/api/images/1/adjust', { ops: { color: { exposure: 900, contrast: -400 } } }, [200],
    r => (r.body?.ops?.color?.exposure === 100 && r.body?.ops?.color?.contrast === -100 && (r.body?.clamped || []).includes('color.exposure') ? '' : `没夹住：${JSON.stringify(r.body).slice(0, 140)}`)],
  ['调整参数坏 JSON 不回 5xx', 'POST', '/api/images/1/adjust', { ops: '不是对象' }, [400, 200],
    r => (r.status === 200 ? (r.body?.ops?.color?.exposure === 0 ? '' : '字段类型不对却没退默认') : '')],
  ['LUT 名字带穿越被拒', 'POST', '/api/images/1/adjust', { ops: { lut: { name: '../../app', strength: 50 } } }, [400]],
  ['预览带 inline 参数（不落库）', 'POST', '/api/images/1/adjust/preview', { ops: { color: { temp: 40 } } }, [200],
    r => (/_adjprev[0-9a-f]{8}\.jpg$/.test(r.body?.preview_url || '') && r.body?.w > 0 ? '' : `预览形状不对：${JSON.stringify(r.body).slice(0, 140)}`)],
  /* 上一条把整条参数链换成了全默认（保存就是整体替换，这是契约的一部分），
     所以这里要重新存一组真参数再落盘——不然测到的是"没参数时拒绝落盘"那条守卫。 */
  ['再存一组真参数（整体替换）', 'POST', '/api/images/1/adjust', { ops: { color: { exposure: 30, clarity: 20 } } }, [200],
    r => (r.body?.ops?.color?.exposure === 30 && r.body?.ops?.color?.sharpen === 0 ? '' : `替换不干净：${JSON.stringify(r.body?.ops?.color).slice(0, 120)}`)],
  ['调整视图的瓦片清单（放大要看真像素）', 'GET', '/api/images/1/adjust/tiles', undefined, [200],
    r => (String(r.body?.url || '').startsWith('/') && r.body?.w > 0 && r.body?.h > 0 && Array.isArray(r.body?.levels) ? '' : `瓦片形状不对：${JSON.stringify(r.body).slice(0, 140)}`)],
  ['成图落盘并回报真实宽高', 'POST', '/api/images/1/adjust/render', {}, [200],
    r => (/_adjusted[0-9a-f]{8}\.jpg$/.test(r.body?.url || '') && r.body?.w > 0 ? '' : `成图形状不对：${JSON.stringify(r.body).slice(0, 140)}`)],
  ['另存为新图挂着父子关系', 'POST', '/api/images/1/adjust/fork', {}, [200],
    r => (r.body?.image_id > 0 && r.body?.derived_from === 1 ? '' : `没挂上父子：${JSON.stringify(r.body).slice(0, 120)}`)],
  ['新图参数起始为空', 'GET', '/api/images/2/adjust', undefined, [200],
    r => (r.body?.ops?.color?.temp === 0 && r.body?.ops?.geometry?.rotate_deg === 0 ? '' : '新图继承了父图参数')],
  ['预览不存在的图 404', 'POST', '/api/images/999/adjust/preview', {}, [404]],
  ['取回画稿缺 result_id 被拒', 'POST', '/api/canvas/1/use-sketch', {}, [400]],
  ['项目设置读写', 'POST', '/api/projects/1/settings', { loras: [{ name: 'x', strength: 1, enabled: true }] }, [200]],
  ['参数 cfg', 'GET', '/api/cfg', undefined, [200]],
  ['版本信息', 'GET', '/api/version', undefined, [200], r => (typeof r.body?.version === 'string' ? '' : '没有版本号')],
  /* 更新日志读的是 GitHub：断网/没发布过都只能回 200 + 空列表或 error，不能把面板打成 5xx */
  ['更新日志（GitHub Releases）', 'GET', '/api/releases', undefined, [200],
    r => (Array.isArray(r.body?.releases) && (typeof r.body?.error === 'string' || r.body?.error == null) ? '' : '形状不对')],
  ['工坊设置', 'GET', '/api/settings', undefined, [200]],
  ['界面语言写入', 'POST', '/api/settings/lang', { lang: 'zh' }, [200]],
  ['工作流路径写入', 'POST', '/api/settings/workflow', { path: 'D:/不存在的目录/wf.json' }, [200]],
  ['角色表读得到', 'GET', '/api/workflow/roles', undefined, [200]],
  ['云端保存', 'POST', '/api/cloud', { kind: 'cloud', base: 'http://127.0.0.1:1/v1', model: 'm-check', key: 'sk-local-check', timeout: '5000', concurrency: 9, stitch_expand: 64, stitch_feather: 200, stitch_edge: 1024 }, [200]],
  ['云端读回', 'GET', '/api/cloud', undefined, [200],
    // key 不往前端发：整个响应体里不能出现那串明文（设置页要显示也得是打码的）
    r => (JSON.stringify(r.body).includes('sk-local-check') ? '把明文 key 发回前端了' : '')],
  ['云端 edit 缺图', 'POST', '/api/cloud/edit', { image_id: 999, mask_b64: PNG, settings: {} }, [404]],
  ['预设 建', 'POST', '/api/presets', { name: '默认预设', prompt: 'p', negative: 'n', steps: 25, cfg: 3, loras: [{ name: 'L', strength: 0.8 }] }, [200]],
  ['预设 重名', 'POST', '/api/presets', { name: '默认预设' }, [400]],
  ['预设 无名字', 'POST', '/api/presets', { name: '   ' }, [400]],
  ['预设 列表', 'GET', '/api/presets', undefined, [200]],
  // 更新与删除的真实行为在「提示词短语 / 预设查重自检」里逐条断言，这里只探"不存在的那一条"
  ['预设 更新 不存在', 'POST', '/api/presets/999999/update', { name: '改名', steps: 30, scope: 'global' }, [404]],
  ['预设 删除 不存在', 'POST', '/api/presets/999999/delete', {}, [404]],
  ['后端列表', 'GET', '/api/backends', undefined, [200]],
  ['后端 登记', 'POST', '/api/backends', { url: '127.0.0.1:9999', label: '冒烟' }, [200]],
  ['后端 移除', 'POST', '/api/backends/remove', { url: 'http://127.0.0.1:9999' }, [200]],
  ['后端 非法地址', 'POST', '/api/backends', { url: 'ftp://x' }, [400]],
  ['导出 未设置', 'GET', '/api/export', undefined, [200], r => (r.body?.ready === false ? '' : '没设置却报可用')],
  ['导出 相对路径拒绝', 'POST', '/api/export/dir', { dir: 'relative/out' }, [400]],
  ['导出 缺 ids', 'POST', '/api/export/run', { result_ids: [] }, [400]],
  ['体检', 'GET', '/api/setup', undefined, [200]],
  ['体检进度', 'GET', '/api/setup/progress', undefined, [200]],
  ['设根目录', 'POST', '/api/setup/root', { path: 'D:/不存在的便携包' }, [200]],
  ['提交无遮罩', 'POST', '/api/run', { image_ids: [1], settings: { prompt: 'x', steps: 20, cfg: 3 } }, [200],
    r => (r.body?.results?.[0]?.skipped === true ? '' : '没遮罩却没跳过')],
  ['提交不存在的图', 'POST', '/api/run', { image_ids: [999], settings: {} }, [200]],
  ['结果 404', 'GET', '/api/results/999', undefined, [404]],
  // 这两条以前带不上 body，被守卫的"json only"挡成 403 就当过了 —— 现在真打到 handler
  ['中断 404', 'POST', '/api/results/999/interrupt', {}, [404]],
  ['删结果 404', 'DELETE', '/api/results/999', undefined, [404]],
  ['fork 404', 'POST', '/api/results/999/fork', {}, [404]],
  ['文件白名单', 'GET', '/file/app.db', undefined, [403],
    // 光看 403 不够：响应体必须是一条错误 JSON，不是那 4KB 的库头
    r => (r.body && typeof r.body.error === 'string' ? '' : `端出去了：${JSON.stringify(r.body).slice(0, 80)}`)],
  ['静态首页', 'GET', '/', undefined, [200], r => (/text\/html/.test(r.ct) ? '' : '不是 HTML')],
  ['静态脚本', 'GET', '/public/js/app.js', undefined, [200]],
  ['静态字体', 'GET', '/public/fonts/NotoSerifSC-VF.woff2', undefined, [200]],
  ['未知路由', 'GET', '/api/nonsense', undefined, [404]],
  ['未知页面', 'GET', '/nope', undefined, [404]],

  /* ===== 回环守卫：GET 这一侧挡的是网页 =====
     写操作已经从 GET 上摘干净（推进在 reclaim::spawn_advancer），剩下的磁盘活靠这两条钉住：
     跨站网页带 Sec-Fetch-Site，本机 harness 与 curl 什么都不带。 */
  ['同源的 GET 照常读', 'GET', '/api/projects/1', undefined, [200], undefined, { 'sec-fetch-site': 'same-origin' }],
  ['跨站网页的 GET 被挡', 'GET', '/api/projects/1', undefined, [403], undefined, { 'sec-fetch-site': 'cross-site' }],
  ['跨站网页驱动的轮询 GET 被挡（曾经会写库）', 'GET', '/api/results/1', undefined, [403], undefined, { 'sec-fetch-site': 'cross-site' }],
  ['不带 Sec-Fetch-Site 的客户端照旧放行', 'GET', '/api/projects/1', undefined, [200]],
  ['跨源的写请求本来就挡', 'POST', '/api/projects/1/rename', { name: 'x' }, [403], undefined, { origin: 'http://evil.example' }],

  /* ===== CSP：桌面壳把四条命令授给了 127.0.0.1 这个远程域，注入进来的脚本得什么都干不成 ===== */
  ['首页带 CSP 且不开 unsafe-inline', 'GET', '/', undefined, [200], r => {
    const p = r.csp || '';
    if (!p) return '没有 CSP 头';
    if (/unsafe-inline|unsafe-eval/.test(p)) return '还留着 unsafe 档：' + p.slice(0, 90);
    return /default-src 'self'/.test(p) && /frame-ancestors 'none'/.test(p) ? '' : '档位不全：' + p.slice(0, 120);
  }],
  ['首帧上色脚本已经在外链里（内联会被 CSP 打死）', 'GET', '/', undefined, [200], async r => {
    // 页面里不能有内联 <script>，且 theme-boot.js 自己取得到
    const html = r.text ?? '';
    if (/<script(?![^>]*\bsrc=)[^>]*>[\s\S]*?<\/script>/.test(html)) return '还有内联脚本';
    const t = await fetch(r.__base + '/public/js/core/theme-boot.js');
    return t.ok ? '' : `theme-boot.js 取不到（${t.status}）`;
  }],
];

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

  /* ---- 界面语言：库里那份是唯一真相（跟着 app_settings 走，重装不丢），坏值夹回 zh 并回显 ---- */
  const langEn = await req(base, 'POST', '/api/settings/lang', { lang: 'en' });
  ok('语言能存成英文并回显', langEn.status === 200 && langEn.body.lang === 'en', JSON.stringify(langEn.body));
  const st2 = await req(base, 'GET', '/api/settings');
  ok('设置接口回显当前语言', st2.body.lang === 'en', JSON.stringify(st2.body).slice(0, 160));
  const langBad = await req(base, 'POST', '/api/settings/lang', { lang: 'fr' });
  ok('没有字典的语言夹回中文', langBad.status === 200 && langBad.body.lang === 'zh', JSON.stringify(langBad.body));
  const dict = await req(base, 'GET', '/public/locales/en.json');
  ok('英文字典读得到且键位齐', dict.status === 200 && dict.body?.nav?.home === 'Home' && dict.body?.shell?.refresh === 'Refresh', JSON.stringify(dict.body || {}).slice(0, 120));
  ok('字典不缓存（升级不会拿着一份旧文案）', /no-store/.test(String(dict.cache)), String(dict.cache));
  const dict404 = await req(base, 'GET', '/public/locales/de.json');
  ok('没有的语言字典不返 200', dict404.status !== 200, `${dict404.status}`);

  // M4 的回归：云端行是 0 步 0 CFG，串回本机这条路的提交必须被挡下而不是跑出一张废图
  const run0 = await req(base, 'POST', '/api/run', { image_ids: [iid], settings: { prompt: 'x', steps: 0, cfg: 0 } });
  const sk0 = (run0.body.results || [])[0] || {};
  ok('本机提交拒收云端形状的 0 步 0 CFG', sk0.skipped === true && sk0.reason === 'srv.comfy.stepsBad', JSON.stringify(run0.body).slice(0, 180));

  // 派生档要跟着图一起删掉，不然目录只涨不落
  await req(base, 'DELETE', `/api/images/${iid}`);
  const pidDir = path.join(dataDir, 'projects', String(p.body.id));
  const filesLeft = fs.existsSync(pidDir) ? fs.readdirSync(pidDir) : ['目录已删'];
  ok('删图连带清掉派生档', filesLeft.length === 0 || filesLeft[0] === '目录已删', JSON.stringify(filesLeft));
  return fails;
}

// ---- M1 项目改名 / 空项目的行为自检 -------------------------------------------
// 改名是纯库操作：断言它一个文件都不碰，比断它回什么更有价值
async function m1SelfCheck(base, dataDir) {
  const fails = [];
  const ok = (name, cond, detail) => {
    console.log(`${cond ? '  ✓' : '  ✗'} ${name}${cond ? '' : '：' + detail}`);
    if (!cond) fails.push(name);
  };
  const p = await req(base, 'POST', '/api/projects', { name: '  带空格  ', files: [{ name: 'a.png', b64: PNG, w: 2, h: 2 }] });
  const pid = p.body.id;
  ok('建项目把名字 trim 掉', p.body.name === '带空格', JSON.stringify(p.body.name));
  const iid0 = p.body.image_ids[0];
  // 后台派生档会往项目目录里补 _thumb.jpg，先等它落定，否则"改名没动文件"会被它绊倒
  for (let i = 0; i < 24 && !(await req(base, 'GET', `/api/images/${iid0}`)).body.thumb_url; i++) {
    await new Promise(r => setTimeout(r, 150));
  }
  const tree = () => {
    const d = path.join(dataDir, 'projects', String(pid));
    return fs.existsSync(d) ? fs.readdirSync(d).sort() : ['目录不存在'];
  };
  const before = tree();
  await new Promise(r => setTimeout(r, 1100));   // updated_at 只有秒级精度，隔一秒才测得出推进（方案 D4）
  const d0 = await req(base, 'GET', `/api/projects/${pid}`);
  const r = await req(base, 'POST', `/api/projects/${pid}/rename`, { name: '改过的名字' });
  ok('改名回 200 与新名', r.status === 200 && r.body.name === '改过的名字', JSON.stringify(r.body));
  ok('改名不动盘上任何文件', JSON.stringify(tree()) === JSON.stringify(before), `${before} → ${tree()}`);
  const d1 = await req(base, 'GET', `/api/projects/${pid}`);
  ok('详情读回新名', d1.body.project?.name === '改过的名字', JSON.stringify(d1.body.project?.name));
  ok('改名推进了 updated_at（首页排序键）', String(d1.body.project?.updated_at) > String(d0.body.project?.updated_at),
    `${d0.body.project?.updated_at} → ${d1.body.project?.updated_at}`);
  const bad = await req(base, 'POST', `/api/projects/${pid}/rename`, { name: '   ' });
  ok('纯空格名被拒 400', bad.status === 400, `${bad.status} ${JSON.stringify(bad.body)}`);
  const gone = await req(base, 'POST', '/api/projects/99999/rename', { name: 'x' });
  ok('改不存在的项目回 404', gone.status === 404, `${gone.status}`);
  const long = await req(base, 'POST', `/api/projects/${pid}/rename`, { name: '长'.repeat(80) });
  ok('超长名字裁到 60 字', long.body.name === '长'.repeat(60), String(long.body.name).length);
  const empty = await req(base, 'POST', '/api/projects', { name: '空项目' });
  ok('零张照片也建得出项目', empty.status === 200 && Array.isArray(empty.body.image_ids) && !empty.body.image_ids.length, JSON.stringify(empty.body).slice(0, 120));
  const ed = await req(base, 'GET', `/api/projects/${empty.body.id}`);
  ok('空项目读得回且不带封面', ed.status === 200 && Array.isArray(ed.body.images) && !ed.body.images.length, JSON.stringify(ed.body).slice(0, 120));
  const nl = await req(base, 'POST', '/api/projects', {});
  ok('没给名字落到兜底显示名', nl.body.name === '未命名项目', JSON.stringify(nl.body.name));
  return fails;
}

// ---- 提示词短语 / 预设查重（方案 M3：短语可自定义 + name_taken 补漏）--------------
async function phraseSelfCheck(base) {
  const fails = [];
  const ok = (name, cond, detail) => {
    console.log(`${cond ? '  ✓' : '  ✗'} ${name}${cond ? '' : '：' + detail}`);
    if (!cond) fails.push(name);
  };
  const seeded = await req(base, 'GET', '/api/presets?kind=phrase');
  const rows = seeded.body || [];
  ok('出厂 10 条短语已播种进库', Array.isArray(rows) && rows.length === 10, `${rows.length} 条`);
  // 出厂句子的三条写法规矩（钉住它，别被后来的润色悄悄改回去）：
  // 一条只讲一个目标状态、正向里不写排除式、皮肤与妆容那两条必须自带"自然"限定
  const zhOnes = rows.filter(r => !/（英文）$/.test(r.name));
  ok('中文短语不带排除式说法', zhOnes.every(r => !/(不要|不许|禁止|切勿|不应|没有)/.test(r.prompt)),
    JSON.stringify(zhOnes.filter(r => /(不要|不许|禁止|切勿|不应|没有)/.test(r.prompt)).map(r => r.name)));
  ok('皮肤与妆容类带克制限定', zhOnes.filter(r => /皮肤|妆/.test(r.name)).every(r => /自然|轻薄|柔和/.test(r.prompt)),
    JSON.stringify(zhOnes.filter(r => /皮肤|妆/.test(r.name)).map(r => r.prompt)));
  const enOne = rows.find(r => /（英文）$/.test(r.name));
  ok('云端那条写明只改遮罩并列出保持清单',
    !!enOne && /Change only the masked area/.test(enOne.prompt) && /exactly the same/.test(enOne.prompt),
    String(enOne && enOne.prompt).slice(0, 80));
  ok('短语列表里不混进参数预设', rows.every(r => r.kind === 'phrase'), JSON.stringify(rows.slice(0, 2).map(r => r.kind)));
  const presets0 = await req(base, 'GET', '/api/presets');
  ok('默认只列预设桶', (presets0.body || []).every(r => r.kind !== 'phrase'), JSON.stringify((presets0.body || []).slice(0, 3).map(r => r.kind)));

  const add = await req(base, 'POST', '/api/presets', { kind: 'phrase', name: '自检短语', prompt: '画面干净一点' });
  ok('新建短语落库', add.status === 200 && add.body.kind === 'phrase' && add.body.name === '自检短语', JSON.stringify(add.body).slice(0, 140));
  const dup = await req(base, 'POST', '/api/presets', { kind: 'phrase', name: '自检短语', prompt: '再一句' });
  ok('同名短语被拒', dup.status === 400, `${dup.status} ${JSON.stringify(dup.body)}`);
  const empty = await req(base, 'POST', '/api/presets', { kind: 'phrase', name: '空的', prompt: '   ' });
  ok('空内容短语被拒', empty.status === 400, `${empty.status} ${JSON.stringify(empty.body)}`);
  const cross = await req(base, 'POST', '/api/presets', { kind: 'preset', name: '自检短语', prompt: 'p' });
  ok('预设与短语分桶，同名不互相挡', cross.status === 200, `${cross.status} ${JSON.stringify(cross.body).slice(0, 120)}`);

  // 改名/换作用域以前完全不查重，于是一条全局 X 和项目里的 X 能并存显示成两条
  const a = await req(base, 'POST', '/api/presets', { name: '甲预设', prompt: 'a' });
  const b = await req(base, 'POST', '/api/presets', { name: '乙预设', prompt: 'b' });
  const clash = await req(base, 'POST', `/api/presets/${b.body.id}/update`, { name: '甲预设', prompt: 'b' });
  ok('改名撞名被拒（以前不查）', clash.status === 400, `${clash.status} ${JSON.stringify(clash.body)}`);
  const scopeClash = await req(base, 'POST', '/api/presets', { name: '乙预设', prompt: 'c', project_id: 5 });
  ok('跨作用域同名也被拒', scopeClash.status === 400, `${scopeClash.status} ${JSON.stringify(scopeClash.body)}`);
  const same = await req(base, 'POST', `/api/presets/${a.body.id}/update`, { name: '甲预设', prompt: 'a2', scope: 'global' });
  ok('自己改自己不算撞名', same.status === 200 && same.body.prompt === 'a2', `${same.status} ${JSON.stringify(same.body).slice(0, 120)}`);

  const long = await req(base, 'POST', '/api/presets', { name: '长文', prompt: '指'.repeat(5000), negative: '负'.repeat(3000) });
  ok('指令与负面有上限', String(long.body.prompt || '').length === 4000 && String(long.body.negative || '').length === 2000,
    `prompt ${String(long.body.prompt || '').length} / negative ${String(long.body.negative || '').length}`);

  // 守卫要求 POST 带 application/json，delete 这类无体请求要给个 {}
  for (const r of [add, cross, a, b, long]) await req(base, 'POST', `/api/presets/${r.body.id}/delete`, {});
  const after = await req(base, 'GET', '/api/presets?kind=phrase');
  ok('删完回到出厂 10 条', (after.body || []).length === 10, `${(after.body || []).length} 条`);
  return fails;
}

// ---- 工作流两种格式 / 节点清点（方案 M7 第一步）----------------------------------
async function workflowSelfCheck(base, dataDir) {
  const fails = [];
  const ok = (name, cond, detail) => {
    console.log(`${cond ? '  ✓' : '  ✗'} ${name}${cond ? '' : '：' + detail}`);
    if (!cond) fails.push(name);
  };
  const apiWf = path.join(dataDir, 'wf-api.json');
  fs.writeFileSync(apiWf, JSON.stringify({
    '3': { class_type: 'UNETLoader', inputs: { unet_name: 'qwen_bf16.safetensors', weight_dtype: 'default' } },
    '8': { class_type: 'CLIPLoader', inputs: { clip_name: 'clipA.safetensors', type: 'qwen' } },
    '9': { class_type: 'VAELoader', inputs: { vae_name: 'vaeA.safetensors' } },
    '11': { class_type: 'TextEncodeQwenImage21', inputs: { prompt: 'API 里的默认正向', negative_prompt: 'API 里的默认负面' } },
    '14': { class_type: 'KSampler', inputs: { model: ['3', 0], seed: 1, steps: 28, cfg: 4.5, sampler_name: 'dpmpp_2m', scheduler: 'karras', denoise: 1 } },
    '4': { class_type: 'LoraLoaderModelOnly', inputs: { model: ['3', 0], lora_name: 'loraX.safetensors', strength_model: 0.66 } },
  }));
  const slashed = apiWf.replace(/\\/g, '/');
  const set = await req(base, 'POST', '/api/settings/workflow', { path: slashed });
  ok('API 导出被认出来', set.body.cfg_source === 'workflow', JSON.stringify(set.body).slice(0, 200));
  const c = (await req(base, 'GET', '/api/cfg')).body;
  ok('参数按名字读到（不再靠控件下标）', c.steps === 28 && c.negative === 'API 里的默认负面', JSON.stringify({ steps: c.steps, negative: c.negative }));
  ok('LoRA 链读得到且带强度', (c.loras || []).length === 1 && c.loras[0].name === 'loraX.safetensors' && c.loras[0].strength === 0.66, JSON.stringify(c.loras).slice(0, 160));
  const insp = await req(base, 'GET', '/api/workflow/inspect');
  ok('清点认出 API 格式与节点数', insp.status === 200 && insp.body.format === 'api' && insp.body.total === 6, JSON.stringify(insp.body).slice(0, 140));
  ok('清点报得出缺缝合那一对', insp.body.stitch_pair_ok === false && (insp.body.known_missing || []).includes('InpaintStitchImproved'), JSON.stringify(insp.body.known_missing));

  // 参数读得到 ≠ 这张图能用来提交：缺必需角色时必须拒绝，而不是悄悄改用内置图
  const roles = await req(base, 'GET', '/api/workflow/roles');
  // 清单里每一条都是 {code, args}：判钥匙与角色，不判句子（措辞改了不该让门红）
  ok('图不完整时拒绝接管而不是回退', roles.status === 200 && roles.body.is_api === true && roles.body.can_takeover === false
    && (roles.body.errors || []).some(e => e.code === 'srv.wf.roleMissing' && e.args?.role?.code === 'wf.role.loadImage'),
    JSON.stringify(roles.body.errors).slice(0, 220));
  ok('角色表回得来这个文件的节点清单', (roles.body.nodes || []).length === 6, JSON.stringify((roles.body.nodes || []).map(x => x.id)));
  const badRole = await req(base, 'POST', '/api/workflow/roles', { roles: { unet: '14' } });
  ok('指错类名的角色被拒并给理由', (badRole.body.rejected || []).length === 1
    && badRole.body.rejected[0].code === 'srv.wf.roleClass' && badRole.body.rejected[0].args?.class === 'UNETLoader', JSON.stringify(badRole.body.rejected));
  const junk = path.join(dataDir, 'wf-junk.json');
  fs.writeFileSync(junk, JSON.stringify({ hello: { world: 1 } }));
  // 这个接口不接路径参数：带了也只会按库里存的那条走（免得变成任意本地文件的读取口）
  const q = await req(base, 'GET', '/api/workflow/inspect?path=' + encodeURIComponent(junk.replace(/\\/g, '/')));
  ok('清点忽略路径参数', q.status === 200 && q.body.format === 'api', `${q.status} ${JSON.stringify(q.body).slice(0, 120)}`);

  // 认得出格式、但一个已知节点都没有：以前这里照样标 workflow，等于谎称参数读自你的文件
  const emptyWf = path.join(dataDir, 'wf-empty.json');
  fs.writeFileSync(emptyWf, JSON.stringify({ nodes: [], links: [] }));
  const set2 = await req(base, 'POST', '/api/settings/workflow', { path: emptyWf.replace(/\\/g, '/') });
  ok('读不到已知节点时不谎称读自工作流', set2.body.cfg_source === 'builtin' && set2.body.cfg_error?.code === 'srv.wf.noParams', JSON.stringify(set2.body).slice(0, 220));
  const set3 = await req(base, 'POST', '/api/settings/workflow', { path: path.join(dataDir, 'nope.json').replace(/\\/g, '/') });
  ok('文件不存在说得不含糊', set3.body.cfg_source === 'builtin' && set3.body.cfg_error?.code === 'srv.wf.fileMissing', JSON.stringify(set3.body).slice(0, 160));
  return fails;
}

async function refsSelfCheck(base, dataDir) {
  const fails = [];
  const ok = (name, cond, detail) => {
    console.log(`${cond ? '  ✓' : '  ✗'} ${name}${cond ? '' : '：' + detail}`);
    if (!cond) fails.push(name);
  };
  const ca = await req(base, 'POST', '/api/canvas/create', { name: '参考图自检', w: 1024, h: 1024 });
  const cb = await req(base, 'POST', '/api/canvas/create', { name: '参考图另一张画布', w: 1024, h: 1024 });
  const a = ca.body.image_id, b = cb.body.image_id;
  ok('两张画布建起来了', !!a && !!b, JSON.stringify({ a, b }));

  const empty = await req(base, 'POST', `/api/canvas/${a}/refs`, {});
  ok('没说要加哪几张就明确拒', empty.status === 400, `${empty.status} ${JSON.stringify(empty.body)}`);
  const junk = await req(base, 'POST', `/api/canvas/${a}/refs`, { files: [{ b64: 'data:image/png;base64,AAAA' }] });
  ok('解不开的参考图被拒而不是落盘', junk.status === 400, `${junk.status} ${JSON.stringify(junk.body)}`);

  // 曾经能读到 app.db 的那族路径形态：槽位集合里出现它们必须被丢掉，且**不动盘上的文件**
  const dbFile = path.join(dataDir, 'app.db');
  const inj = await req(base, 'PUT', `/api/canvas/${a}/refs`, { paths: ['../../app.db', 'app.db', '/etc/passwd', 'projects/999/x.png'] });
  ok('注入型 rel 进不了集合', inj.status === 200 && (inj.body.refs || []).length === 0, `${inj.status} ${JSON.stringify(inj.body)}`);
  ok('跟着坏集合删文件这条路被挡住（库还在盘上）', fs.existsSync(dbFile), 'app.db 被删了');

  const tooMany = await req(base, 'PUT', `/api/canvas/${a}/refs`, { paths: Array.from({ length: 9 }, (_, i) => `projects/1/x${i}.png`) });
  ok('整组替换超上限被拒', tooMany.status === 400, `${tooMany.status} ${JSON.stringify(tooMany.body)}`);

  const gc = await req(base, 'POST', `/api/canvas/${b}/generate`, { settings: { prompt: '另一张画布的那一版' } });
  const ridB = gc.body.result_id;
  const cross = await req(base, 'POST', `/api/canvas/${a}/refs`, { from_result: ridB });
  ok('别的画布的记录不能往这张上搬参考图', cross.status === 400 && cross.body?.code === 'srv.canvas.wrongOwner',
    `${cross.status} ${JSON.stringify(cross.body)}`);

  const one = await req(base, 'GET', `/api/canvas/${a}`);
  ok('画布详情带得上槽位集合与上限', Array.isArray(one.body.refs) && one.body.refs_max >= 1, JSON.stringify(one.body).slice(0, 160));

  /* 非相邻重复：`Vec::dedup()` 只去相邻的，body 给 ["a","b","a"] 时同一张参考图会被发两次。
     反向对照就是这条——以前留下的集合是 3 张而不是 2 张。 */
  const two = await req(base, 'POST', `/api/canvas/${a}/refs`, { files: [{ b64: PNG }, { b64: PNG }] });
  const paths = (two.body.refs || []).map(r => r.path);
  const dup = await req(base, 'PUT', `/api/canvas/${a}/refs`, { paths: paths.length >= 2 ? [paths[0], paths[1], paths[0]] : paths });
  const kept = (dup.body.refs || []).map(r => r.path);
  ok('整组替换里非相邻的重复只算一张', dup.status === 200 && kept.length === 2 && new Set(kept).size === 2,
    `${dup.status} 留下 ${kept.length} 张`);
  return fails;
}

/**
 * 数据目录独占（回归：双开毁图那条链路）。
 * 第二份进程要是跑通了，它会按自己那份空白的在飞表把第一份正在跑的成图判成僵尸——
 * 所以这里看的不是"退没退"，而是盘上什么都没变。
 * 顺带钉住"port.txt 已经取消"：起服务的人自己知道端口，谁都不再往数据根目录落这个文件。
 */
async function lockSelfCheck(base, dataDir) {
  const fails = [];
  const ok = (name, cond, detail) => {
    console.log(`${cond ? '  ✓' : '  ✗'} ${name}${cond ? '' : '：' + detail}`);
    if (!cond) fails.push(name);
  };
  ok('数据根目录不再有 port.txt', !fs.existsSync(path.join(dataDir, 'port.txt')), '还在：端口不该再往盘上落一份');
  ok('锁在 runtime/ 里', fs.existsSync(path.join(dataDir, 'runtime', 'instance.lock')), path.join(dataDir, 'runtime', 'instance.lock'));
  const proc = spawn(BIN, [], { cwd: ROOT, env: { ...process.env, SYNCO_DATA: dataDir, SYNCO_PORT: '0' }, stdio: ['ignore', 'pipe', 'pipe'] });
  let out = '';
  proc.stdout.on('data', b => { out += b.toString(); });
  proc.stderr.on('data', b => { out += b.toString(); });
  const code = await new Promise(res => {
    const t = setTimeout(() => { proc.kill(); res('15 秒没退'); }, 15000);
    proc.on('exit', c => { clearTimeout(t); res(c); });
  });
  ok('第二份进程被挡在开库之前（非 0 退出）', typeof code === 'number' && code !== 0, `退出码 ${code}｜${out.slice(0, 120)}`);
  ok('理由说的是这个目录已经在用', /已经有另一个 Synco/.test(out), out.slice(0, 160));
  ok('第二份没把 SYNCO_URL 打出来（没绑成端口）', !/SYNCO_URL=/.test(out), out.slice(0, 160));
  const alive = await req(base, 'GET', '/api/projects');
  ok('第一份照常服务', alive.status === 200, `${alive.status}`);
  return fails;
}

async function main() {
  assert(fs.existsSync(BIN), `先 cargo build -p synco-server（找不到 ${BIN}）`);
  const dataDir = tmpData();
  const { proc, base, log } = await boot(dataDir);
  console.log(`Rust ${base.replace(/^http:\/\/127\.0\.0\.1:/, '')} @ ${dataDir}\n`);

  let bad = 0;
  const record = process.argv.includes('--record');
  console.log(record ? '接口扫查（--record：只报回来的状态码）' : '接口扫查（每条认状态码，不是"没崩就算过"）');
  for (const [title, method, url, body, expect, check, extra] of SWEEP) {
    let r;
    try { r = await req(base, method, url, body, extra); } catch (e) { bad++; console.log(`✗ ${title}  抛了 ${e.message}`); continue; }
    if (record) { console.log(`  ${r.status}  ${title}`); continue; }
    let why = '';
    if (!Array.isArray(expect) || !expect.includes(r.status)) why = `期望 ${(expect || []).join('/') || '?'}，回来 ${r.status}`;
    if (!why && check) why = (await check(r)) || '';
    if (why) bad++;
    console.log(`${why ? '✗' : '✓'} ${title}  (${r.status})${why ? ' ' + why + '｜' + JSON.stringify(r.body).slice(0, 150) : ''}`);
  }
  if (record) {
    proc.kill();
    await new Promise(r => setTimeout(r, 400));
    try { fs.rmSync(dataDir, { recursive: true, force: true }); } catch {}
    console.log('\n（--record 只跑扫查这一遍，行为断言没跑）');
    return;
  }

  console.log('\n路径穿越（回归：这几条曾经能读到 app.db）');
  const tv = await traversalCheck(base);

  console.log('\nM1 项目改名 / 空项目自检');
  const m1 = await m1SelfCheck(base, dataDir);

  console.log('\nM3 服务端图像化 / 队列自检');
  const m3 = await m3SelfCheck(base, dataDir);

  console.log('\n提示词短语 / 预设查重自检');
  const ph = await phraseSelfCheck(base);

  console.log('\n工作流两种格式 / 节点清点自检');
  const wf = await workflowSelfCheck(base, dataDir);

  console.log('\n画布参考图槽位自检');
  const rf = await refsSelfCheck(base, dataDir);

  console.log('\n数据目录独占自检');
  const lk = await lockSelfCheck(base, dataDir);
  proc.kill();
  if (bad || m1.length || m3.length || ph.length || wf.length || rf.length || lk.length || tv.length) console.log('\n—— 服务输出 ——\n' + log.join('').split('\n').slice(-40).join('\n'));
  if (!process.argv.includes('--keep')) {
    // 进程还在退的时候 Windows 会锁着目录，等一会儿再删，删不掉也不算失败
    await new Promise(r => setTimeout(r, 600));
    try { fs.rmSync(dataDir, { recursive: true, force: true }); } catch { console.log(`（临时目录留着了：${dataDir}）`); }
  }
  console.log(`\n失败计数 → 端点扫查 ${bad} · 穿越 ${tv.length} · M1 ${m1.length} · M3 ${m3.length} · 短语 ${ph.length} · 工作流 ${wf.length} · 参考图 ${rf.length} · 目录锁 ${lk.length}`);
  process.exit(bad || tv.length || m1.length || m3.length || ph.length || wf.length || rf.length || lk.length ? 1 : 0);
}

main().catch(e => { console.error(e); process.exit(2); });
