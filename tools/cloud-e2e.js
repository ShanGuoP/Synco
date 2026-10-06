// M3 端到端：合成 24MP PNG → 导入 → 交 proxy 分辨率遮罩 → 服务端队列 → 假云端原样回图
// → 服务端缝合落盘 → 校验"蒙版外逐像素没被动过"，最后杀进程重启验队列续跑。
//
// 假云端收在 127.0.0.1 随机端口，你的 API key 完全不参与这条链路；
// 它把我们发过去的那张裁切图原样退回（identity），所以蒙版外应当与原图像素全等。
//
//   node tools/cloud-e2e.js            # 全程自己起服务，跑完自己收
//   node tools/cloud-e2e.js --keep     # 保留临时 DATA 便于复查
'use strict';
const { spawn, execFileSync } = require('child_process');
const zlib = require('zlib');
const fs = require('fs');
const os = require('os');
const http = require('http');
const path = require('path');

const ROOT = path.join(__dirname, '..');
const RUST_BIN = path.join(ROOT, 'target', 'debug', 'synco.exe');
const W = 4000, H = 6000;                      // 就是你 data/ 里那个规格
const INK = { cx: 2000, cy: 3000, r: 420 };    // 涂抹区（原图坐标）
const SAFE_R = INK.r + 1400;                   // 裁切框 = 涂抹外扩 + 上下文留白，取样一律避开

// ---- 最小 PNG 编码（RGBA / filter 0），够造可复现的合成图 -----------------------
const CRC = (() => {
  const t = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c;
  }
  return t;
})();
function crc32(buf) {
  let c = ~0;
  for (let i = 0; i < buf.length; i++) c = CRC[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return ~c;
}
function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const t = Buffer.from(type, 'ascii');
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([t, data])) >>> 0);
  return Buffer.concat([len, t, data, crc]);
}
function png(w, h, rgbaAt) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8; ihdr[9] = 6;
  const raw = Buffer.alloc(h * (1 + w * 4));
  for (let y = 0; y < h; y++) {
    const row = y * (1 + w * 4);
    for (let x = 0; x < w; x++) raw.set(rgbaAt(x, y), row + 1 + x * 4);
  }
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr), chunk('IDAT', zlib.deflateSync(raw, { level: 6 })), chunk('IEND', Buffer.alloc(0)),
  ]);
}
const pngSize = b => ({ w: b.readUInt32BE(16), h: b.readUInt32BE(20) });
/** JPEG 的 SOF 段里有宽高：派生档是 .jpg，不能用 PNG 的头去读 */
function jpegSize(b) {
  if (b[0] !== 0xff || b[1] !== 0xd8) throw new Error('不是 JPEG');
  let i = 2;
  while (i + 9 < b.length) {
    if (b[i] !== 0xff) { i++; continue; }
    const m = b[i + 1];
    const len = b.readUInt16BE(i + 2);
    // C0..CF，跳过 C4（DAC）与 C8（JPG）
    if (m >= 0xc0 && m <= 0xcf && m !== 0xc4 && m !== 0xc8) return { h: b.readUInt16BE(i + 5), w: b.readUInt16BE(i + 7) };
    i += 2 + len;
  }
  throw new Error('找不到 JPEG 的 SOF 段');
}
const imgSize = b => (b[1] === 0x50 ? pngSize(b) : jpegSize(b));

// ---- 假云端：收到 /v1/images/edits 就把 multipart 里的 image 段退回 ------------
function startMock() {
  const hits = [];
  const srv = http.createServer((req, res) => {
    const bufs = [];
    req.on('data', b => bufs.push(b));
    req.on('end', () => {
      const body = Buffer.concat(bufs);
      hits.push({ url: req.url, bytes: body.length });
      const start = body.indexOf(Buffer.from([0x89, 0x50, 0x4e, 0x47]));
      if (start < 0) { res.writeHead(400, { 'content-type': 'application/json' }); return res.end('{"error":"mock 没收到 PNG"}'); }
      const end = body.indexOf('\r\n--', start);
      const pngBytes = body.subarray(start, end < 0 ? body.length : end);
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ data: [{ b64_json: pngBytes.toString('base64') }] }));
    });
  });
  return new Promise(res => srv.listen(0, '127.0.0.1', () => res({ srv, port: srv.address().port, hits })));
}

// ---- 起一个隔离实例：临时 DATA + 随机端口，绝不碰 7861 --------------------------
function boot(dataDir) {
  try { fs.unlinkSync(path.join(dataDir, 'port.txt')); } catch { /* 首轮没有 */ }
  const proc = spawn(RUST_BIN, [], { cwd: ROOT, env: { ...process.env, SYNCO_DATA: dataDir, SYNCO_PORT: '0' }, stdio: ['ignore', 'pipe', 'pipe'] });
  const log = [];
  proc.stdout.on('data', b => log.push(b.toString()));
  proc.stderr.on('data', b => log.push(b.toString()));
  const t0 = Date.now();
  return new Promise((res, rej) => {
    const tick = () => {
      let p = null;
      try { p = fs.readFileSync(path.join(dataDir, 'port.txt'), 'utf8').trim(); } catch { /* 还没写 */ }
      if (p && /^\d+$/.test(p)) return res({ proc, log, base: `http://127.0.0.1:${p}` });
      if (proc.exitCode !== null) return rej(new Error('工坊启动就退了：\n' + log.join('')));
      if (Date.now() - t0 > 20000) return rej(new Error('等 port.txt 超时'));
      setTimeout(tick, 120);
    };
    tick();
  });
}

async function req(base, method, p, body) {
  const r = await fetch(base + p, body === undefined
    ? { method, signal: AbortSignal.timeout(15000) }
    : { method, headers: { 'content-type': 'application/json' }, body: JSON.stringify(body), signal: AbortSignal.timeout(15000) });
  const ct = r.headers.get('content-type') || '';
  return { status: r.status, body: ct.includes('json') ? await r.json().catch(() => null) : Buffer.from(await r.arrayBuffer()) };
}
const statusOf = async (base, id) => (await req(base, 'GET', `/api/results/${id}`)).body.status;
async function waitRows(base, ids, want, ms = 200000) {
  const t0 = Date.now();
  let st = [];
  while (Date.now() - t0 < ms) {
    st = await Promise.all(ids.map(id => statusOf(base, id)));
    if (st.every(s => want.includes(s))) return st;
    await new Promise(r => setTimeout(r, 250));
  }
  return st;
}

const fails = [];
function check(name, cond, detail) {
  console.log(`${cond ? '  ✓' : '  ✗'} ${name}${cond ? '' : '：' + detail}`);
  if (!cond) fails.push(name);
}

async function main() {
  if (!fs.existsSync(RUST_BIN)) throw new Error(`先 cargo build -p synco-server（找不到 ${RUST_BIN}）`);
  const data = path.join(os.tmpdir(), `synco-e2e-${Date.now()}`);
  fs.mkdirSync(data, { recursive: true });
  const mock = await startMock();
  let srv = await boot(data);
  console.log(`工坊 ${srv.base} · 假云端 http://127.0.0.1:${mock.port} · DATA ${data}\n`);

  const photo = png(W, H, (x, y) => [(x * 255 / W) | 0, (y * 255 / H) | 0, ((x + y) * 255 / (W + H)) | 0, 255]);
  check('合成原图是真 24MP', pngSize(photo).w === W && pngSize(photo).h === H, JSON.stringify(pngSize(photo)));
  const proj = await req(srv.base, 'POST', '/api/projects', {
    name: '端到端', files: [{ name: 'big.png', b64: 'data:image/png;base64,' + photo.toString('base64'), w: W, h: H }],
  });
  const iid = proj.body.image_ids[0];
  check('导入 24MP 建项目', proj.status === 200 && !!iid, JSON.stringify(proj.body));

  let info = {};
  for (let i = 0; i < 200 && !info.proxy_url; i++) {
    info = (await req(srv.base, 'GET', `/api/images/${iid}`)).body;
    if (!info.proxy_url) await new Promise(r => setTimeout(r, 200));
  }
  check('24MP 切出 proxy 档', /_proxy3072\.jpg$/.test(String(info.proxy_url || '')), JSON.stringify(info.proxy_url));
  const pd = imgSize((await req(srv.base, 'GET', info.proxy_url)).body);
  check('proxy 长边就是 3072', Math.max(pd.w, pd.h) === 3072, JSON.stringify(pd));
  const scale = pd.w / W;

  // 遮罩按 proxy 分辨率交：M3 之后浏览器不再交满尺寸 PNG
  const mask = png(pd.w, pd.h, (x, y) => {
    const dx = x / scale - INK.cx, dy = y / scale - INK.cy;
    return dx * dx + dy * dy < INK.r * INK.r ? [255, 255, 255, 255] : [0, 0, 0, 0];
  });
  check('proxy 分辨率遮罩被收下', (await req(srv.base, 'POST', `/api/images/${iid}/mask`, { b64: 'data:image/png;base64,' + mask.toString('base64') })).status === 200, 'POST mask 非 200');
  const back = (await req(srv.base, 'GET', (await req(srv.base, 'GET', `/api/images/${iid}`)).body.mask_url)).body;
  check('落盘的遮罩仍是 proxy 尺寸（上采样归服务端）', pngSize(back).w === pd.w && pngSize(back).h === pd.h, JSON.stringify(pngSize(back)));

  await req(srv.base, 'POST', '/api/cloud', {
    kind: 'cloud', base: `http://127.0.0.1:${mock.port}/v1`, model: 'mock-identity', key: 'sk-mock-only',
    size: '', quality: '', concurrency: '2',
  });

  const q = await req(srv.base, 'POST', '/api/cloud/queue', { image_ids: [iid, iid], settings: { prompt: '把涂到的地方调亮' } });
  const rows = (q.body.results || []).map(r => r.result_id);
  check('批量入队建了 2 行', rows.length === 2 && (q.body.skipped || []).length === 0, JSON.stringify(q.body).slice(0, 200));
  const st = await waitRows(srv.base, rows, ['done', 'error']);
  check('两行都落到 done', st.every(s => s === 'done'), st.join(','));
  check('假云端被调到（并发 2 就两发）', mock.hits.length === 2, `hits=${mock.hits.length}`);
  check('发出去的是裁切区而不是整图', mock.hits.every(h => h.bytes < photo.length * 0.6), mock.hits.map(h => h.bytes).join(','));

  const done = (await req(srv.base, 'GET', `/api/results/${rows[0]}`)).body;
  check('成图带 320 缩略档（历史列不再解码 24MP）', /_thumb\.jpg$/.test(String(done.thumb_url || '')), JSON.stringify(done.thumb_url));
  const finalBuf = (await req(srv.base, 'GET', done.final_url)).body;
  check('成图尺寸与原图一致', pngSize(finalBuf).w === W && pngSize(finalBuf).h === H, JSON.stringify(pngSize(finalBuf)));

  // 零漂移：两个 PNG 交给 System.Drawing 逐点比，取样全在涂抹与裁切留白之外
  const f1 = path.join(data, 'orig.png'), f2 = path.join(data, 'final.png');
  fs.writeFileSync(f1, photo);
  fs.writeFileSync(f2, finalBuf);
  const ps = [
    "$ErrorActionPreference='Stop'",
    'Add-Type -AssemblyName System.Drawing',
    '$a=[System.Drawing.Bitmap]::new($env:P1)',
    '$b=[System.Drawing.Bitmap]::new($env:P2)',
    '$cx=[int]$env:CX;$cy=[int]$env:CY;$rr=[int]$env:RR',
    '$bad=0;$n=0;$sx=7;$sy=11',
    'for($i=0;$i -lt 3000;$i++){',
    '  $sx=(($sx*1103515245+12345) -band 0x7fffffff); $sy=(($sy*1103515245+12345) -band 0x7fffffff)',
    '  $x=$sx % $a.Width; $y=$sy % $a.Height',
    '  if ([math]::Sqrt(($x-$cx)*($x-$cx)+($y-$cy)*($y-$cy)) -lt $rr) { continue }',
    '  $n++; $pa=$a.GetPixel($x,$y); $pb=$b.GetPixel($x,$y)',
    '  if ($pa.R -ne $pb.R -or $pa.G -ne $pb.G -or $pa.B -ne $pb.B) { $bad++ }',
    '}',
    '"$bad/$n"',
    '$a.Dispose();$b.Dispose()',
  ].join('\n');
  const out = execFileSync('powershell', ['-NoProfile', '-Command', ps], {
    encoding: 'utf8',
    env: { ...process.env, P1: f1, P2: f2, CX: String(INK.cx), CY: String(INK.cy), RR: String(SAFE_R) },
  }).trim().split(/\r?\n/).pop();
  const [bad, n] = out.split('/').map(Number);
  check(`蒙版外逐像素不动（取样 ${n} 点，避开半径 ${SAFE_R}px）`, n > 100 && bad === 0, `不符 ${bad}/${n}`);

  // 重启续跑：排两行下去，中途杀进程，再起——queued 该接着跑，在飞的那次判掉
  const q2 = await req(srv.base, 'POST', '/api/cloud/queue', { image_ids: [iid, iid, iid], settings: { prompt: '重启后还要跑完' } });
  const rows2 = (q2.body.results || []).map(r => r.result_id);
  await new Promise(r => setTimeout(r, 400));
  srv.proc.kill();
  await new Promise(r => setTimeout(r, 500));
  srv = await boot(data);
  const st2 = await waitRows(srv.base, rows2, ['done', 'error'], 90000);
  check('重启后 queued 行被接回去跑到终态', st2.length === 3 && st2.every(s => s === 'done' || s === 'error'), st2.join(','));
  check('续跑确实又调了云端', mock.hits.length > 2, `hits=${mock.hits.length}`);

  srv.proc.kill();
  if (!process.argv.includes('--keep')) {
    await new Promise(r => setTimeout(r, 600));
    try { fs.rmSync(data, { recursive: true, force: true }); } catch { console.log(`（临时目录留着了：${data}）`); }
  }
  mock.srv.close();
  console.log(`\n端到端失败：${fails.length} 项${fails.length ? ' → ' + fails.join('；') : ''}`);
  process.exit(fails.length ? 1 : 0);
}

main().catch(e => { console.error(e); process.exit(2); });
