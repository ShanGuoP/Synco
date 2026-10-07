// M0 探针：图生图到底走哪条契约，只有真打一次才知道（假云端是 identity 回图，测不出模型行为）。
//
// 一次跑三种打法，把回来的图存到一起供人眼判：
//   甲 edits + 不带 mask   —— 画稿当 image，整幅按草稿生成（Synco 现在这条路）
//   乙 edits + 全透明 mask —— 画稿当 image，遮罩说"整幅都要重绘"
//   丙 generations 只给提示词 —— 纯文生图，画稿不参与（对照组）
//
//   node tools/sketch-probe.js --out D:/tmp/sketch-probe          # 只造画稿并打印将要发什么
//   node tools/sketch-probe.js --out ... --base https://... --model gpt-image-2 --key sk-... --yes
//
// key 只从命令行或环境变量取，绝不读本机库里的明文 key；不加 --yes 不会发出任何请求。
// 每一发都是真计费：先只跑一次（不带 --yes）看清请求长什么样。
'use strict';
const fs = require('fs');
const os = require('os');
const path = require('path');
const zlib = require('zlib');

const W = 1024, H = 1024;
const PROMPT = '一张写实照片：画面中央一个红色球体，左侧一个蓝色立方体，右侧一个绿色圆锥，三者立在浅灰桌面上，同一背景为柔和的米色渐变，自然光从左上方来，阴影落在右下方';

function arg(name, fb) {
  const i = process.argv.indexOf('--' + name);
  return i > -1 && process.argv[i + 1] ? process.argv[i + 1] : fb;
}
const BASE = (arg('base', process.env.SYNCO_BASE) || '').replace(/\/+$/, '');
const MODEL = arg('model', process.env.SYNCO_MODEL) || '';
const KEY = arg('key', process.env.SYNCO_KEY) || '';
const GO = process.argv.includes('--yes');
const OUT = path.resolve(arg('out', path.join(os.tmpdir(), `synco-sketch-probe-${Date.now()}`)));
const REFS = process.argv.includes('--refs');            // 多参考图那一族（不是原来的甲乙丙三发）
const ALL = process.argv.includes('--all');              // 三种形状全跑（默认回图到手就停，省额度）
const SKETCHLESS = process.argv.includes('--sketchless'); // 画稿可空：只发参考图
const ONLY_SHAPE = arg('shape', '');

// ---- 最小 PNG 编码（RGBA / filter 0），够造一张干净的画稿 ------------------------
function crc32(buf) {
  let c = ~0;
  const t = [];
  for (let n = 0; n < 256; n++) {
    let x = n;
    for (let k = 0; k < 8; k++) x = x & 1 ? 0xedb88320 ^ (x >>> 1) : x >>> 1;
    t[n] = x;
  }
  for (const b of buf) c = t[(c ^ b) & 255] ^ (c >>> 8);
  return ~c >>> 0;
}
function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([len, body, crc]);
}
/** rgbaAt 返回 [r,g,b,a]；深色墨 + 透明底，与 Synco 存的画稿同构 */
function encodePng(w, h, rgbaAt) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8; ihdr[9] = 6;
  const raw = Buffer.alloc(h * (1 + w * 4));
  for (let y = 0; y < h; y++) {
    const off = y * (1 + w * 4);
    raw[off] = 0;
    for (let x = 0; x < w; x++) raw.set(rgbaAt(x, y), off + 1 + x * 4);
  }
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr), chunk('IDAT', zlib.deflateSync(raw, { level: 9 })), chunk('IEND', Buffer.alloc(0)),
  ]);
}

const near = (v, t, tol) => Math.abs(v - t) <= tol;
function sketchAt(x, y) {
  const clear = [0, 0, 0, 0];
  const ink = [26, 25, 23, 255];
  // 轮廓而不是实心：探针要测的是"模型看不看得懂草稿的摆位"，实心块会把"参考草稿"和"照描"混成一件事
  const line = (d) => (Math.abs(d) <= 9 ? ink : null);
  // 地平线 + 三个圆（与提示词里的摆位一致），故意画得潦草
  if (near(y, H * 0.72, 8)) return ink;
  const shapes = [
    [W * 0.5, H * 0.52, 150],   // 中央球
    [W * 0.24, H * 0.6, 120],   // 左立方体（画成方框）
    [W * 0.76, H * 0.58, 130],  // 右圆锥（画成三角）
  ];
  const [cx, cy, r] = shapes[0];
  if (line(Math.hypot(x - cx, y - cy) - r)) return ink;
  const [bx, by, bs] = shapes[1];
  if ((near(x - bs, bx, 8) || near(x + bs, bx, 8)) && Math.abs(y - by) <= bs) return ink;
  if ((near(y - bs, by, 8) || near(y + bs, by, 8)) && Math.abs(x - bx) <= bs) return ink;
  const [tx, ty, tr] = shapes[2];
  const half = tr * ((y - (ty - tr)) / (2 * tr));
  if (y > ty - tr && y < ty + tr && (near(Math.abs(x - tx), Math.max(0, half), 9))) return ink;
  if (near(y - tr, ty, 8) && Math.abs(x - tx) <= tr) return ink;
  return clear;
}

/** 全透明遮罩：edits 的语义里"透明=重绘"，整幅透明就是整幅重绘 */
function blankMask(w, h, opaque) {
  return encodePng(w, h, () => (opaque ? [255, 255, 255, 255] : [0, 0, 0, 0]));
}

/* ---- 多参考图：字段形状没实测过，只有真打一次才知道 --------------------------------
   两张参考内容刻意差得远（红底白圆 / 蓝底白方）：HTTP 码只说明"收不收"，
   回图里两个物体都在才说明"真用了"。尺寸也按合法档来（1024²，16 倍数、像素在框内），
   不然分不清是"多图形状不对"还是"输入尺寸越界"被拒。 */
const REF_PROMPT = '按两张参考图里的物体拍一张写实照片：左边放参考图一里的红色球体，右边放参考图二里的蓝色立方体。两者立在浅灰桌面上，自然光从左上方来。';
const refA = encodePng(W, H, (x, y) => (Math.hypot(x - W / 2, y - H / 2) < W * 0.28 ? [255, 255, 255, 255] : [190, 32, 28, 255]));
const refB = encodePng(W, H, (x, y) => (Math.abs(x - W / 2) < W * 0.2 && Math.abs(y - H / 2) < H * 0.2 ? [255, 255, 255, 255] : [24, 56, 168, 255]));
const SHAPES = [
  { k: 'brackets', label: 'image[] 同名多 part', name: () => 'image[]' },
  { k: 'repeat', label: 'image 同名多 part', name: () => 'image' },
  { k: 'indexed', label: 'image_1 / image_2 递增', name: i => `image_${i + 1}` },
];

function form({ image, mask, files, prompt, size, model }) {
  const b = `----syncoProbe${Date.now()}`;
  const parts = [];
  const field = (name, value) => Buffer.from(`--${b}\r\nContent-Disposition: form-data; name="${name}"\r\n\r\n${value}\r\n`);
  const file = (name, filename, buf) => Buffer.concat([
    Buffer.from(`--${b}\r\nContent-Disposition: form-data; name="${name}"; filename="${filename}"\r\nContent-Type: image/png\r\n\r\n`),
    buf,
    Buffer.from('\r\n'),
  ]);
  parts.push(field('model', model));
  parts.push(field('prompt', prompt));
  parts.push(field('size', size));
  parts.push(field('response_format', 'b64_json'));
  if (image) parts.push(file('image', 'sketch.png', image));
  // 多参考图的三种候选形状都从这里出：files 是 [字段名, 文件名, 字节]
  for (const [name, filename, buf] of files || []) parts.push(file(name, filename, buf));
  if (mask) parts.push(file('mask', 'mask.png', mask));
  parts.push(Buffer.from(`--${b}--\r\n`));
  return { body: Buffer.concat(parts), type: `multipart/form-data; boundary=${b}` };
}

async function send(endpoint, fields, tag) {
  const f = form(fields);
  const url = `${BASE}${endpoint}`;
  const t0 = Date.now();
  const res = await fetch(url, {
    method: 'POST',
    headers: { authorization: `Bearer ${KEY}`, 'content-type': f.type },
    body: f.body,
    signal: AbortSignal.timeout(300000),
  });
  const text = await res.text();
  let j = null;
  try { j = JSON.parse(text); } catch { /* 不是 JSON 就原样留证 */ }
  const one = j?.data?.[0] || {};
  let bytes = null;
  if (one.b64_json) bytes = Buffer.from(one.b64_json, 'base64');
  else if (one.url) {
    const r2 = await fetch(one.url, { signal: AbortSignal.timeout(120000) });
    if (r2.ok) bytes = Buffer.from(await r2.arrayBuffer());
  }
  const file = bytes ? path.join(OUT, `${tag}.png`) : null;
  if (bytes) fs.writeFileSync(file, bytes);
  const note = {
    打法: tag, 端点: endpoint, 带图: !!fields.image, 带遮罩: !!fields.mask,
    文件字段: (fields.files || []).map(f => f[0]).join(',') || null,
    HTTP: res.status, 毫秒: Date.now() - t0, 请求字节: f.body.length,
    回图: file ? path.basename(file) : null,
    错误: bytes ? null : text.slice(0, 400),
  };
  fs.writeFileSync(path.join(OUT, `${tag}.json`), JSON.stringify(note, null, 2));
  console.log(`  ${bytes ? '✓' : '✗'} ${tag}  HTTP ${res.status}  ${Date.now() - t0}ms  ${bytes ? `→ ${path.basename(file)}` : text.slice(0, 160)}`);
  return bytes;
}

async function probeRefs(sketch) {
  const filesOf = sh => [refA, refB].map((buf, i) => [sh.name(i), `ref${i + 1}.png`, buf]);
  const order = ONLY_SHAPE ? SHAPES.filter(s => s.k === ONLY_SHAPE) : SHAPES;
  if (!order.length) throw new Error(`--shape 只认 ${SHAPES.map(s => s.k).join(' / ')}`);

  console.log(`多参考图探针：${SKETCHLESS ? '只发两张参考（不带画稿）' : '画稿 + 两张参考'}，输出档 ${W}x${H}，共 ${order.length} 种候选形状\n`);
  for (const sh of order) {
    const f = filesOf(sh);
    const imgBytes = f[0][2].length * 2 + (SKETCHLESS ? 0 : sketch.length);
    console.log(`  ${sh.label} → 字段 ${f.map(x => x[0]).join(' / ')}；参考每张 ${f[0][2].length} 字节${SKETCHLESS ? '' : `，画稿 ${sketch.length} 字节`}；图像负载合计 ${imgBytes} 字节`);
  }
  if (!GO) {
    console.log('\n没有 --yes，所以一发都没发。要真打（每种形状都是真计费）：');
    console.log(`  node tools/sketch-probe.js --refs --out "${OUT}" --base <你的中转 /v1> --model <模型名> --key sk-… --yes`);
    console.log('  默认第一种形状拿到回图就停；三种都要就加 --all；只试一种 --shape brackets|repeat|indexed；');
    console.log('  要测"画稿可空、只发参考图"那一读法就加 --sketchless。');
    return;
  }
  if (!BASE || !MODEL || !KEY) throw new Error('缺 --base / --model / --key 之一');
  for (const sh of order) {
    const got = await send('/images/edits', {
      prompt: REF_PROMPT, size: `${W}x${H}`, model: MODEL,
      image: SKETCHLESS ? null : sketch, files: filesOf(sh),
    }, `refs-${sh.k}${SKETCHLESS ? '-noSketch' : ''}`);
    if (got && !ALL) { console.log(`\n认了：${sh.label}。别种形状没试，额度省下。`); return; }
  }
  console.log('\n怎么看：回图里红球与蓝方**都在**才算真吃到两张参考；只出现一个（或颜色被换掉）说明它只取了其中一份，' +
    '那多图得换形状，或者退回"服务端先把几张拼成一张"这条路。');
}

async function main() {
  fs.mkdirSync(OUT, { recursive: true });
  const sketch = encodePng(W, H, sketchAt);
  const sketchFile = path.join(OUT, 'sketch.png');
  fs.writeFileSync(sketchFile, sketch);
  fs.writeFileSync(path.join(OUT, 'sketch-mask-opaque.png'), blankMask(W, H, true));
  console.log(`画稿：${sketchFile}  ${W}×${H}  ${sketch.length} 字节\n提示词：${PROMPT.slice(0, 40)}…\n输出目录：${OUT}\n`);

  if (REFS) return probeRefs(sketch);

  if (!GO) {
    console.log('没有 --yes，所以一发都没发。要真打（每发都计费）：');
    console.log(`  node tools/sketch-probe.js --out "${OUT}" --base <你的中转 /v1> --model <模型名> --key sk-… --yes`);
    return;
  }
  if (!BASE || !MODEL || !KEY) throw new Error('缺 --base / --model / --key 之一');
  const common = { prompt: PROMPT, size: `${W}x${H}`, model: MODEL };
  console.log('三发对照（甲是 Synco 现在的打法）：');
  await send('/images/edits', { ...common, image: sketch, mask: null }, 'jia-edits-no-mask');
  await send('/images/edits', { ...common, image: sketch, mask: blankMask(W, H, false) }, 'yi-edits-clear-mask');
  await send('/images/generations', { ...common, image: null, mask: null }, 'bing-generations-only');
  console.log(`\n怎么看：把 sketch.png 与三张回图并排看——
  甲像草稿的"精修版" → Synco 现在的链路就是对的；
  甲完全不参考草稿、只有丙像 → 该换 generations + 把草稿当参考图重想；
  乙回图四周留白/发灰 → 全透明遮罩被当成了"整幅重绘但保住底色"，别用乙。`);
}

main().catch(e => { console.error(String(e && e.stack || e)); process.exit(2); });
