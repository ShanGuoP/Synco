// 0.3「本地调整」端到端：起一个隔离实例（临时 DATA + 随机端口），导一张真的图进去，
// 把七个动作逐个跑一遍并断言产物：存参数 / 预览 / 成图 / 另存为新图 / 提交输入 / 删图清理。
//
// 为什么非要自己起实例：这条链会往磁盘写派生档（缓存层 `runtime/cache/`，加上提交记录那一张
// 落在项目目录），跑在你正在用的那个 DATA 上就会把它的缓存搅进测试的缓存。
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
const BIN = process.env.SYNCO_BIN || path.join(ROOT, 'target', 'debug', 'synco.exe');
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

/* JPEG 头里的真实宽高：SOF 段（FFC0–FFCF，跳过 DHT/CIP）后第 5–6 字节是高、7–8 是宽。
   用来把"接口报的尺寸"与"盘上那张的分辨率"分开验——两者相等就等于没验到 proxy 档那一条。 */
function jpegSize(file) {
  if (!fs.existsSync(file)) return null;
  const b = fs.readFileSync(file);
  for (let i = 2; i + 9 < b.length; i++) {
    if (b[i] !== 0xff) continue;
    const m = b[i + 1];
    if (m >= 0xc0 && m <= 0xcf && m !== 0xc4 && m !== 0xc8 && m !== 0xcc) {
      return { h: b.readUInt16BE(i + 5), w: b.readUInt16BE(i + 7) };
    }
    const len = b.readUInt16BE(i + 2);
    if (!(len > 1)) continue;
    i += len + 1;
  }
  return null;
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

    /* proxy 档降到 1024：预览那条链只有在"proxy 比源图小"时才会露出尺寸口径的问题，
       0.3.0 正是这么坏掉的——报的是 proxy 档自己的宽高，前端拿它当了画幅 */
    const pe = await req(base, 'POST', '/api/settings/proxy-edge', { proxy_edge: 1024 });
    ok('隔离实例把 proxy 档降到 1024', pe.body?.proxy_edge === 1024, JSON.stringify(pe.body));

    /* ---- 导一张 1600×1200 的图（长边超出 proxy 档），等派生档补齐 ---- */
    const pngBuf = png(1600, 1200);
    const proj = await req(base, 'POST', '/api/projects', { name: '调整链路', files: [{ name: 'sample.png', b64: pngBuf.toString('base64'), w: 1600, h: 1200 }] });
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
    ok('源图比 proxy 档大（这条链路要的就是这种图）', Math.max(dims.w, dims.h) > 1024, JSON.stringify(dims));

    /* ---- 0. 没参数时"调整瓦片"就等于源图那一套：前端不必分辨屏幕上摆的是哪一张 ---- */
    const t0 = await req(base, 'GET', `/api/images/${imgId}/adjust/tiles`);
    const src0 = await req(base, 'GET', `/api/images/${imgId}/tiles`);
    ok('空参数时调整瓦片回的是源图那套', t0.status === 200 && t0.body?.url === src0.body?.url, `${JSON.stringify(t0.body).slice(0, 160)} vs ${src0.body?.url}`);

    /* ---- 1. GET adjust：无记录 = 全默认 ---- */
    const g0 = await req(base, 'GET', `/api/images/${imgId}/adjust`);
    ok('GET adjust 回全默认', g0.status === 200 && g0.body.ops?.color?.exposure === 0 && g0.body.ops?.v === 1, JSON.stringify(g0.body).slice(0, 200));
    ok('面板数据带预设与 LUT 名单', Array.isArray(g0.body.presets) && g0.body.presets.length >= 8 && Array.isArray(g0.body.luts), JSON.stringify(g0.body.presets || []).slice(0, 80));
    // 摆滑杆的数值必须由后端下发：前端曾经自己抄一份 PRESET_VALUES，两份一漂就是"界面摆在 A、出图是 B"
    const ps0 = g0.body.presets || [];
    ok('每条预设都带滑杆数值', ps0.every(p => p.color && typeof p.color.exposure === 'number'), JSON.stringify(ps0[0] || {}).slice(0, 120));
    const mono0 = ps0.find(p => p.id === 'mono') || {};
    ok('下发值与 color::preset 同一张表', mono0.color && mono0.color.saturation === -100 && mono0.color.contrast === 40, JSON.stringify(mono0.color || {}));

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
    ok('预览返回 URL 与宽高', p1.status === 200 && /\/cache\/s\/\d+\/adjprev[0-9a-f]{8}\.jpg$/.test(prevRel) && p1.body.w === dims.w, JSON.stringify(p1.body).slice(0, 200));
    const prevFile = path.join(data, prevRel);
    ok('预览档真的在盘上', fs.existsSync(prevFile), prevFile);
    // 分开验两件事：接口报的是**成图**尺寸，盘上那张确实还是 proxy 分辨率。
    // 只验前一句的话，把 build_preview 改回报 out.w 也照样过（那就是 0.3.0 的原始缺陷）
    const prevPx = jpegSize(prevFile);
    ok('预览档本身是 proxy 分辨率（报出去的却是成图尺寸）', !!prevPx && prevPx.w === 1024 && p1.body.w === dims.w, `${JSON.stringify(prevPx)} vs ${p1.body.w}×${p1.body.h}`);
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

    /* ---- 4b. 放大要看真像素：调整视图的瓦片切的是成图那一档，不是 proxy ---- */
    await req(base, 'POST', `/api/images/${imgId}/adjust`, { ops: { color: { exposure: 60, clarity: 30 } } });
    const t1 = await req(base, 'GET', `/api/images/${imgId}/adjust/tiles`);
    const turl = t1.body?.url || '';
    ok('调整瓦片报成图尺寸（与画幅同一个数）', t1.status === 200 && t1.body?.w === dims.w && t1.body?.h === dims.h, JSON.stringify(t1.body).slice(0, 200));
    ok('瓦片目录按参数指纹归位', /\/cache\/t\/\d+\/adj[0-9a-f]{8}$/.test(turl.split('/{z}/')[0]), turl);
    const lv = t1.body?.levels || [];
    ok('成图切出了金字塔', t1.body?.tile === 512 && lv.length > 1, JSON.stringify(lv).slice(0, 160));
    if (lv.length) {
      const fine = lv[lv.length - 1];
      const one = await req(base, 'GET', turl.replace('{z}', String(fine.z)).replace('{x}', '0').replace('{y}', '0'));
      ok('最细一层的瓦片取得到且可长期缓存', one.status === 200 && /immutable/.test(one.cache || ''), `${one.status} ${one.cache}`);
    }
    // 换一套参数 = 换一套目录，旧的那套要被请走：一次上百张，留着就是白堆磁盘
    await req(base, 'POST', `/api/images/${imgId}/adjust`, { ops: { color: { exposure: -30, clarity: 30 } } });
    const t2 = await req(base, 'GET', `/api/images/${imgId}/adjust/tiles`);
    const adjDir = t1.body.url.replace('/file/', '').split('/{z}/')[0];
    const rootRel = path.dirname(adjDir.split('/').join(path.sep));
    const still = fs.existsSync(path.join(data, rootRel)) ? fs.readdirSync(path.join(data, rootRel)).filter(n => n.startsWith('adj')) : [];
    ok('换参数后只留当前这一套瓦片', t2.body?.url !== turl && still.length === 1, `${path.join(data, rootRel)} → ${still.join(',') || '（没有 adj 目录）'}`);

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

    /* ---- 7b. 美颜那四根新杆（0.3.x 追加）：夹逼、缺省补 0、质感不算改过。
       算子本身的作用范围与阈值由 photoedit-core 的单测钉（样张跨阈值两侧），
       这里只钉接口这条线：字段名、量程、默认值、以及"要不要白渲染一张"。 ---- */
    const sNew = await req(base, 'POST', `/api/images/${imgId}/adjust`, { ops: { beauty: { smooth: 30, texture: 300, blemish: -20, even_tone: 88, de_shine: 140, sharpen: 10 } } });
    ok('新字段越界逐个夹逼并记账',
      ['beauty.texture', 'beauty.blemish', 'beauty.de_shine'].every(k => (sNew.body?.clamped || []).includes(k)) && sNew.body.ops.beauty.texture === 100 && sNew.body.ops.beauty.blemish === 0,
      JSON.stringify(sNew.body).slice(0, 260));
    const gNew = await req(base, 'GET', `/api/images/${imgId}/adjust`);
    ok('新字段存得回、读得到', gNew.body.ops.beauty.even_tone === 88 && gNew.body.ops.beauty.de_shine === 100 && gNew.body.ops.beauty.smooth === 30, JSON.stringify(gNew.body.ops.beauty));
    const gOld = await req(base, 'POST', `/api/images/${imgId}/adjust`, { ops: { color: { exposure: 20 } } });
    ok('不带新字段的老参数按默认补 0（老库不用迁）',
      [gOld.body.ops.beauty.texture, gOld.body.ops.beauty.blemish, gOld.body.ops.beauty.even_tone, gOld.body.ops.beauty.de_shine].every(v => v === 0),
      JSON.stringify(gOld.body.ops.beauty));
    await req(base, 'POST', `/api/images/${imgId}/adjust`, { ops: { beauty: { texture: 70 } } });
    const pTex = await req(base, 'POST', `/api/images/${imgId}/adjust/preview`, {});
    ok('只拖质感不算改过：预览走恒等直出', pTex.body?.identity === true, JSON.stringify(pTex.body).slice(0, 200));
    await req(base, 'POST', `/api/images/${imgId}/adjust`, { ops: { beauty: { smooth: 40, texture: 30, blemish: 60, even_tone: 50, de_shine: 45, by_mask: true } } });
    const pMask = await req(base, 'POST', `/api/images/${imgId}/adjust/preview`, {});
    const rNew = await req(base, 'POST', `/api/images/${imgId}/adjust/render`, {});
    ok('四根新杆一起开也出得了预览与成图', pMask.status === 200 && rNew.status === 200 && /\/cache\/s\/\d+\/adjusted[0-9a-f]{8}\.jpg$/.test(rNew.body?.url || ''), `${pMask.status}/${JSON.stringify(rNew.body).slice(0, 140)}`);
    const tiled = await req(base, 'GET', `/api/images/${imgId}/adjust/tiles`);
    ok('新算子那版也能切真像素瓦片', tiled.status === 200 && tiled.body?.w === dims.w, JSON.stringify(tiled.body).slice(0, 160));


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
    // 缓存层按 image_id 归位：整目录请走之后不该再留这张图的瓦片与渲染档
    ok('缓存层里这张图的两处跟着没了',
      !fs.existsSync(path.join(data, 'runtime', 'cache', 's', String(imgId))) && !fs.existsSync(path.join(data, 'runtime', 'cache', 't', String(imgId))),
      path.join(data, 'runtime', 'cache'));
    const kidFile = path.join(data, String(kidInfo?.orig_url || '').replace('/file/', ''));
    ok('删父图没碰子图的原图', !!kidInfo?.orig_url && fs.existsSync(kidFile), kidFile);
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
