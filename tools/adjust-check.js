// 0.3「本地调整」端到端：起一个隔离实例（临时 DATA + 随机端口），导一张真的图进去，
// 把七个动作逐个跑一遍并断言产物：存参数 / 预览 / 成图 / 另存为新图 / 提交输入 / 删图清理。
//
// 为什么非要自己起实例：这条链会往磁盘写派生档，跑在你正在用的那个 DATA 上
// 就会在你的项目目录里留下真的 _adjprev/_adjusted 文件。
//
//   node tools/adjust-check.js            # 跑完删掉临时 DATA
//   node tools/adjust-check.js --keep     # 留着临时 DATA 便于复查产物
'use strict';
const { spawn } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');
const zlib = require('zlib');

const ROOT = path.join(__dirname, '..');
const BIN = path.join(ROOT, 'target', 'debug', 'synco.exe');
const fails = [];
const ok = (name, cond, detail) => {
  console.log(`${cond ? '  ✓' : '  ✗'} ${name}${cond ? '' : '：' + String(detail).slice(0, 300)}`);
  if (!cond) fails.push(name);
};

/* ---------- 造一张能看出调整的图（纯 zlib 写 PNG，不引依赖） ---------- */
function png(W, H) {
  const bytes = w => [w >> 8 & 255, w & 255];
  const rows = [];
  for (let y = 0; y < H; y++) {
    const row = Buffer.alloc(1 + W * 3);
    for (let x = 0; x < W; x++) {
      const u = x / W, v = y / H;
      let r = 60 + 150 * u, g = 40 + 90 * v, b = 150 - 90 * u;
      if (u > 0.42 && u < 0.52) { r = 235; g = 230; b = 225; }            // 竖亮带
      if (v > 0.62) { r = 40 + 60 * u; g = 60 + 80 * u; b = 120 + 90 * (1 - u); }   // 下半冷色
      const n = 14 * Math.sin(x * 0.35 + y * 0.11);                        // 细噪声：磨皮要吃掉它
      const o = 1 + x * 3;
      row[o] = Math.max(0, Math.min(255, r + n));
      row[o + 1] = Math.max(0, Math.min(255, g + n));
      row[o + 2] = Math.max(0, Math.min(255, b + n));
    }
    rows.push(row);
  }
  const chunk = (type, data) => {
    const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
    const crcBuf = Buffer.concat([Buffer.from(type, 'ascii'), data]);
    const c = Buffer.alloc(4); c.writeUInt32BE(zlib.crc32 ? zlib.crc32(crcBuf) : crc32(crcBuf));
    return Buffer.concat([len, Buffer.from(type, 'ascii'), data, c]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(W, 0); ihdr.writeUInt32BE(H, 4);
  ihdr[8] = 8; ihdr[9] = 2; ihdr[10] = 0; ihdr[11] = 0; ihdr[12] = 0;
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk('IHDR', ihdr), chunk('IDAT', zlib.deflateSync(Buffer.concat(rows), { level: 6 })), chunk('IEND', Buffer.alloc(0)),
  ]);
}
// node 24 有 zlib.crc32；没有就自带一份多项式表版
function crc32(buf) {
  let c, table = crc32.t || (crc32.t = (() => {
    const t = new Int32Array(256);
    for (let n = 0; n < 256; n++) { let x = n; for (let k = 0; k < 8; k++) x = x & 1 ? 0xedb88320 ^ (x >>> 1) : x >>> 1; t[n] = x; }
    return t;
  })());
  c = ~0;
  for (const b of buf) c = (c >>> 8) ^ table[(c ^ b) & 255];
  return ~c >>> 0;
}

/* ---------- 隔离实例 ---------- */
function tmpData() {
  const d = fs.mkdtempSync(path.join(os.tmpdir(), 'synco-adj-e2e-'));
  if (!fs.realpathSync(d).includes('synco-adj-e2e')) throw new Error('临时目录不对劲：' + d);
  return d;
}
async function req(base, method, p, body) {
  const opt = { method, headers: {}, signal: AbortSignal.timeout(30000) };
  if (body !== undefined) { opt.headers['content-type'] = 'application/json'; opt.body = JSON.stringify(body); }
  const r = await fetch(base + p, opt);
  const ct = r.headers.get('content-type') || '';
  const val = ct.includes('json') ? await r.json().catch(() => null) : { __text: (await r.text()).slice(0, 120) };
  return { status: r.status, etag: r.headers.get('etag'), cache: r.headers.get('cache-control'), body: val };
}
const sleep = ms => new Promise(r => setTimeout(r, ms));

async function main() {
  if (!fs.existsSync(BIN)) { console.log(`缺少 ${BIN}：先 cargo build -p synco-server`); process.exit(1); }
  const data = tmpData();
  const port = 20000 + Math.floor(Math.random() * 20000);
  const base = `http://127.0.0.1:${port}`;
  const child = spawn(BIN, [], { env: { ...process.env, SYNCO_DATA: data, SYNCO_PORT: String(port) }, stdio: ['ignore', 'ignore', 'pipe'] });
  let err = '';
  child.stderr.on('data', b => { err += b.toString(); });
  try {
    for (let i = 0; i < 60; i++) {
      try { const r = await fetch(`${base}/api/version`, { signal: AbortSignal.timeout(700) }); if (r.ok) break; } catch { }
      await sleep(300);
    }
    console.log(`隔离实例 :${port}  DATA ${data}\n`);

    /* ---- 导一张 900×600 的图，等派生档补齐 ---- */
    const pngBuf = png(900, 600);
    const proj = await req(base, 'POST', '/api/projects', { name: '调整链路', files: [{ name: 'sample.png', b64: pngBuf.toString('base64'), w: 900, h: 600 }] });
    const imgId = proj.body?.image_ids?.[0];
    ok('导入一张图', !!imgId, JSON.stringify(proj.body));
    let info = null;
    for (let i = 0; i < 40; i++) {
      info = (await req(base, 'GET', `/api/images/${imgId}`)).body;
      if (info?.proxy_url || info?.thumb_url) break;
      await sleep(250);
    }
    ok('派生档已就位', !!(info && (info.thumb_url || info.proxy_url)), JSON.stringify(info || {}).slice(0, 200));
    const dims = { w: info.w, h: info.h };

    /* ---- 1. GET adjust：无记录 = 全默认 ---- */
    const g0 = await req(base, 'GET', `/api/images/${imgId}/adjust`);
    ok('GET adjust 回全默认', g0.status === 200 && g0.body.ops?.color?.exposure === 0 && g0.body.ops?.v === 1, JSON.stringify(g0.body).slice(0, 200));
    ok('面板数据带预设与 LUT 名单', Array.isArray(g0.body.presets) && g0.body.presets.length >= 8 && Array.isArray(g0.body.luts), JSON.stringify(g0.body.presets || []).slice(0, 80));

    /* ---- 2. POST adjust：越界要夹逼并记账 ---- */
    const s1 = await req(base, 'POST', `/api/images/${imgId}/adjust`, { ops: { color: { exposure: 900, contrast: -30 }, beauty: { smooth: 45, by_mask: false } } });
    ok('越界值被夹住并报字段名', s1.body?.clamped?.includes('color.exposure') && s1.body?.ops?.color?.exposure === 100, JSON.stringify(s1.body).slice(0, 220));
    const g1 = await req(base, 'GET', `/api/images/${imgId}/adjust`);
    ok('存进去的读得回来', g1.body.ops.color.contrast === -30 && g1.body.beauty_saved !== false && g1.body.ops.beauty.smooth === 45, JSON.stringify(g1.body.ops).slice(0, 220));

    /* ---- 3. 坏 JSON 与被拒的 LUT 名 ---- */
    const bad = await req(base, 'POST', `/api/images/${imgId}/adjust`, { ops: { color: '乱填' } });
    ok('字段类型不对按默认吞掉', bad.status === 200 && bad.body.ops.color.exposure === 0, JSON.stringify(bad.body).slice(0, 160));
    const badLut = await req(base, 'POST', `/api/images/${imgId}/adjust`, { ops: { lut: { name: '../../app', strength: 100 } } });
    ok('穿越型 LUT 名被拒', badLut.status === 400, `${badLut.status} ${JSON.stringify(badLut.body)}`);

    /* ---- 4. 预览：落档 + 同参数复用 + 换参数换名 ---- */
    await req(base, 'POST', `/api/images/${imgId}/adjust`, { ops: { color: { exposure: 60, clarity: 30 } } });
    const p1 = await req(base, 'POST', `/api/images/${imgId}/adjust/preview`, {});
    const prevRel = (p1.body?.preview_url || '').replace('/file/', '');
    ok('预览返回 URL 与宽高', p1.status === 200 && prevRel.includes('_adjprev') && p1.body.w === dims.w, JSON.stringify(p1.body).slice(0, 200));
    const prevFile = path.join(data, prevRel);
    ok('预览档真的在盘上', fs.existsSync(prevFile), prevFile);
    const sz1 = fs.existsSync(prevFile) ? fs.statSync(prevFile).size : 0;
    const p2 = await req(base, 'POST', `/api/images/${imgId}/adjust/preview`, {});
    ok('同参数二次预览复用同一份（不重算）', p2.body.reused === true && p2.body.preview_url === p1.body.preview_url, JSON.stringify(p2.body).slice(0, 160));
    const cache = (await req(base, 'GET', p1.body.preview_url)).cache || '';
    ok('预览档可长期缓存（名字里带参数指纹，换参数就换 URL）', /immutable/.test(cache), cache);
    await req(base, 'POST', `/api/images/${imgId}/adjust`, { ops: { color: { exposure: -60, clarity: 30 } } });
    const p3 = await req(base, 'POST', `/api/images/${imgId}/adjust/preview`, {});
    const prevRel3 = (p3.body?.preview_url || '').replace('/file/', '');
    ok('换参数就是另一个文件', prevRel3 !== prevRel && !fs.existsSync(prevFile), `${prevRel} → ${prevRel3}`);
    ok('旧预览档被请走（目录不堆版本）', !fs.existsSync(prevFile), prevFile);

    /* ---- 5. 几何：转 90° 之后成图换边长 ---- */
    await req(base, 'POST', `/api/images/${imgId}/adjust`, { ops: { geometry: { rotate_deg: 90, crop: null, flip_h: false, flip_v: false, fill: 'edge' } } });
    const r1 = await req(base, 'POST', `/api/images/${imgId}/adjust/render`, {});
    ok('成图按几何段换边长', r1.status === 200 && r1.body.w === dims.h && r1.body.h === dims.w, JSON.stringify(r1.body).slice(0, 200));
    ok('成图带缩略档', !!r1.body.thumb_url && fs.existsSync(path.join(data, r1.body.thumb_url.replace('/file/', ''))), JSON.stringify(r1.body).slice(0, 200));

    /* ---- 6. 另存为新图：谱系挂上、新图参数为空、派生档自己补 ---- */
    const f1 = await req(base, 'POST', `/api/images/${imgId}/adjust/fork`, {});
    const kid = f1.body?.image_id;
    ok('另存为新图返回 image_id', !!kid, JSON.stringify(f1.body).slice(0, 200));
    const kidInfo = (await req(base, 'GET', `/api/images/${kid}`)).body;
    ok('新图挂着父子关系', kidInfo?.derived_from === imgId && kidInfo?.w === dims.h && kidInfo?.h === dims.w, JSON.stringify({ d: kidInfo?.derived_from, w: kidInfo?.w, h: kidInfo?.h }));
    const kidOps = await req(base, 'GET', `/api/images/${kid}/adjust`);
    ok('新图参数起始为空', kidOps.body.ops.color.exposure === 0 && kidOps.body.ops.beauty.smooth === 0, JSON.stringify(kidOps.body.ops).slice(0, 160));
    ok('父图的调整还在（另存是复制不是转移）', (await req(base, 'GET', `/api/images/${imgId}/adjust`)).body.ops.geometry.rotate_deg === 90);

    /* ---- 7. 液化笔画：入库前抽稀 + 重放一致 ---- */
    const straight = Array.from({ length: 60 }, (_, i) => [0.2 + i * 0.008, 0.5]);
    await req(base, 'POST', `/api/images/${imgId}/adjust`, { ops: { geometry: {}, warp: { strokes: [{ tool: 'push', points: straight, radius: 0.08, strength: 70 }] } } });
    const saved = await req(base, 'GET', `/api/images/${imgId}/adjust`);
    const pts = saved.body.ops.warp.strokes[0].points;
    ok('直线轨迹入库前被抽稀', pts.length <= 3, `${straight.length} → ${pts.length}`);
    const w1 = await req(base, 'POST', `/api/images/${imgId}/adjust/preview`, {});
    const w2 = await req(base, 'POST', `/api/images/${imgId}/adjust/preview`, {});
    ok('同参数预览指向同一档（可重放）', w1.body.preview_url === w2.body.preview_url, `${w1.body.preview_url} vs ${w2.body.preview_url}`);

    /* ---- 8. 接口形状与"画稿不走这条链路" ---- */
    const none = await req(base, 'GET', `/api/images/${imgId}`);
    ok('图片详情没被调整层改动形状', ['id', 'w', 'h', 'orig_url', 'mask_url', 'thumb_url', 'proxy_url', 'tiles_url'].every(k => k in none.body), Object.keys(none.body).join(','));
    // 画布走的还是云端那一档的像素门槛（1024² 是 api-check 也在用的合法尺寸）
    const sketch = await req(base, 'POST', '/api/canvas/create', { w: 1024, h: 1024, name: '画一张', project_id: proj.body.id });
    const sid = sketch.body?.image_id ?? sketch.body?.id;
    const sketchAdjust = await req(base, 'GET', `/api/images/${sid}/adjust`);
    ok('画稿不走本地调整', sid > 0 && sketchAdjust.status === 400, `${sketch.status}/${sid} → ${sketchAdjust.status} ${JSON.stringify(sketchAdjust.body || sketch.body).slice(0, 90)}`);

    /* ---- 8. 提交给 AI 重绘的输入 = 调整后那张（拍板 4）。
       后端故意指到一个死端口：不花额度、不给他 8188 塞任务，
       但 submit_artifact 在上传之前就跑完了，所以"该发出去的那一张有没有落盘"仍然可断言。 ---- */
    const dead = '127.0.0.1:9998';
    await req(base, 'POST', '/api/backends', { url: dead, label: '死的' });
    await req(base, 'POST', '/api/backends/select', { url: dead });
    // 两张图都要有遮罩才会真走到提交那一步（没遮罩在入口就跳过了，测不到输入域）
    await req(base, 'POST', `/api/images/${imgId}/mask`, { b64: png(8, 8).toString('base64') });
    // 新图（无参数）提交：不该产生任何 adjinput
    const runPlain = await req(base, 'POST', '/api/run', { image_ids: [kid], settings: { prompt: 'x', steps: 20, cfg: 3 } });
    const afterPlain = fs.readdirSync(path.join(data, 'projects', '1')).filter(n => /_adjinput/.test(n));
    ok('没参数的图提交不产生 adjinput（零回归）', afterPlain.length === 0, `${afterPlain.join(',')}｜${JSON.stringify(runPlain.body).slice(0, 120)}`);
    // 给父图存一组真参数再提交：应当落一张"实际发出去的那一张"
    await req(base, 'POST', `/api/images/${imgId}/adjust`, { ops: { color: { temp: 45 } } });
    const runR = await req(base, 'POST', '/api/run', { image_ids: [imgId], settings: { prompt: 'x', steps: 20, cfg: 3 } });
    const inputs = fs.readdirSync(path.join(data, 'projects', '1')).filter(n => /_adjinput/.test(n));
    ok('调整过的图提交时落了 adjinput', inputs.length === 1, `${inputs.join(',')}｜${JSON.stringify(runR.body).slice(0, 160)}`);
    ok('提交没当成崩掉（后端是死的，回跳过或上游错误都算对）',
      runR.status === 200 && JSON.stringify(runR.body).length > 2, JSON.stringify(runR.body).slice(0, 160));

    /* ---- 9. 删图连带清掉参数与调整档 ---- */
    const del = await req(base, 'DELETE', `/api/images/${imgId}`);
    ok('删图成功', del.status === 200, JSON.stringify(del.body));
    const dirNow = fs.readdirSync(path.join(data, 'projects', '1'));
    ok('调整档随图清掉', !dirNow.some(n => /_adj(prev|usted|thumb|input)/.test(n)), dirNow.filter(n => /_adj/.test(n)).join(','));
    ok('附属行跟着没了（再读参数=全默认）', (await req(base, 'GET', `/api/images/${imgId}/adjust`)).status === 404);
    console.log(`\n删图后项目目录剩 ${dirNow.length} 个条目`);
  } catch (e) {
    fails.push('异常');
    console.log('抛异常：', e && e.stack || e);
  } finally {
    child.kill();
    await sleep(400);
    if (process.argv.includes('--keep')) console.log('保留临时 DATA：' + data);
    else fs.rmSync(data, { recursive: true, force: true, maxRetries: 6, retryDelay: 150 });
  }
  console.log('\n本地调整端到端失败：' + fails.length + ' 项' + (fails.length ? ' → ' + fails.join('、') : ''));
  process.exit(fails.length ? 1 : 0);
}
main();
