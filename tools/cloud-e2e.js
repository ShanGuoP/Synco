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
      // 记下 form 里的 size 与有没有 mask part：断言"尺寸胶囊真落到这一枪"与"画稿不带遮罩"要靠它
      const text = body.toString('latin1');
      const size = /name="size"\r\n\r\n([^\r\n]+)/.exec(text);
      const prompt = /name="prompt"\r\n\r\n([^\r\n]*)/.exec(text);
      const hit = {
        url: req.url, bytes: body.length,
        size: size ? size[1] : null,
        hasMask: /name="mask"/.test(text),
        prompt: prompt ? prompt[1] : '',
      };
      hits.push(hit);
      // 每个 image part 都是一个 PNG 段：多参考图之后要把**每一份**都记下来，
      // 不然"发出去的是不是这一行的快照"只看得到第一份，剩下几张悄悄漏了也不知道
      const sig = Buffer.from([0x89, 0x50, 0x4e, 0x47]);
      const images = [];
      for (let at = body.indexOf(sig); at >= 0; at = body.indexOf(sig, at + 1)) {
        const end = body.indexOf('\r\n--', at);
        images.push(body.subarray(at, end < 0 ? body.length : end));
        if (end < 0) break;
      }
      if (!images.length) { res.writeHead(400, { 'content-type': 'application/json' }); return res.end('{"error":"mock 没收到 PNG"}'); }
      const pngBytes = images[0];
      hit.image = pngBytes;
      hit.images = images;
      // 反向那一次要回一张"完全不一样"的图，才验得出保住的主体确实没被动过
      let reply = pngBytes;
      if (/INVERT/.test(prompt ? prompt[1] : '')) {
        const iw = pngBytes.readUInt32BE(16), ih = pngBytes.readUInt32BE(20);
        reply = png(iw, ih, () => [0, 255, 0, 255]);
      }
      const send = () => {
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(JSON.stringify({ data: [{ b64_json: reply.toString('base64') }] }));
      };
      // HOLD：把唯一那个坑占住，好让下一行确实排在队列里（排队期间改画稿才测得出读的是哪份）
      if (/HOLD/.test(prompt ? prompt[1] : '')) setTimeout(send, 2000); else send();
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

  // 项目页角标的数据源：结果数与最新成图档由项目详情一次算出来（旧的 has_result 没有任何端点会发）
  const pj = (await req(srv.base, 'GET', `/api/projects/${proj.body.id}`)).body;
  const irow = (pj.images || []).find(x => x.id === iid) || {};
  check('项目详情带真实结果数', irow.result_count === 2 && irow.result_done === 2, JSON.stringify({ count: irow.result_count, done: irow.result_done }));
  check('角标用的最新成图小档给得出', /_thumb\.jpg$/.test(String(irow.latest_result_url || '')), JSON.stringify(irow.latest_result_url));
  const col = await req(srv.base, 'GET', `/api/results?project_id=${proj.body.id}`);
  check('结果集合端点列得出整项目', col.status === 200 && (col.body.results || []).length === 2, JSON.stringify(col.body).slice(0, 160));
  const one = await req(srv.base, 'GET', `/api/results?image_id=${iid}&limit=1`);
  check('集合能按单图收窄并尊重 limit', (one.body.results || []).length === 1 && one.body.truncated === true, JSON.stringify(one.body).slice(0, 160));
  const noArgs = await req(srv.base, 'GET', '/api/results');
  check('集合缺参数时明说而不是回全库', noArgs.status === 400, `${noArgs.status} ${JSON.stringify(noArgs.body)}`);

  // 尺寸胶囊（方案的 D3）：以前前端把它写进 settings，服务端从来没人读
  const hitBefore = mock.hits.length;
  const qEdge = await req(srv.base, 'POST', '/api/cloud/queue', { image_ids: [iid], settings: { prompt: '把涂到的地方调亮', edge: 2048 } });
  const ridEdge = (qEdge.body.results || [])[0]?.result_id;
  const stEdge = await waitRows(srv.base, [ridEdge], ['done', 'error']);
  const sizes = mock.hits.slice(hitBefore).map(h => h.size);
  const baseSize = mock.hits[hitBefore - 1]?.size;
  check('这一枪按 2048 档出图（edge 落到请求上）', stEdge[0] === 'done' && parseInt(String(sizes[sizes.length - 1])) > parseInt(String(baseSize)),
    `默认 ${baseSize} → edge=2048 ${sizes.join(',')}`);
  check('没带 edge 的请求仍按设置里的 stitch_edge', String(baseSize || '').startsWith('1024'), JSON.stringify(baseSize));
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

  // ---- 反向涂抹（M5）：涂住的主体逐字节不动，其余整幅交给模型。这次 mock 回的是纯绿 ----
  const qInv = await req(srv.base, 'POST', '/api/cloud/queue', { image_ids: [iid], settings: { prompt: 'INVERT 反向圈住主体', invert: true } });
  const ridInv = (qInv.body.results || [])[0]?.result_id;
  const stInv = await waitRows(srv.base, [ridInv], ['done', 'error']);
  check('反向提交能跑到终态', stInv[0] === 'done', JSON.stringify(stInv));
  const invHit = mock.hits[mock.hits.length - 1];
  check('反向那次仍带遮罩（涂了就有保留区）', !!invHit && invHit.hasMask === true, JSON.stringify(invHit));
  if (stInv[0] === 'done') {
    const inv = (await req(srv.base, 'GET', `/api/results/${ridInv}`)).body;
    const invBuf = (await req(srv.base, 'GET', inv.final_url)).body;
    const f3 = path.join(data, 'invert.png');
    fs.writeFileSync(f3, invBuf);
    const psInv = [
      "$ErrorActionPreference='Stop'",
      'Add-Type -AssemblyName System.Drawing',
      '$a=[System.Drawing.Bitmap]::new($env:P1)',
      '$b=[System.Drawing.Bitmap]::new($env:P2)',
      '$cx=[int]$env:CX;$cy=[int]$env:CY;$rr=[int]$env:RR;$rin=[int]$env:RIN',
      '$keep=0;$nin=0;$chg=0;$nout=0;$sx=13;$sy=7',
      'for($i=0;$i -lt 12000;$i++){',
      '  $sx=(($sx*1103515245+12345) -band 0x7fffffff); $sy=(($sy*1103515245+12345) -band 0x7fffffff)',
      '  $x=$sx % $a.Width; $y=$sy % $a.Height',
      '  $d=[math]::Sqrt(($x-$cx)*($x-$cx)+($y-$cy)*($y-$cy))',
      '  $pa=$a.GetPixel($x,$y); $pb=$b.GetPixel($x,$y)',
      '  $same=($pa.R -eq $pb.R -and $pa.G -eq $pb.G -and $pa.B -eq $pb.B)',
      '  if ($d -lt $rin) { $nin++; if ($same) { $keep++ } }',
      '  elseif ($d -gt ($rr + 400)) { $nout++; if (-not $same) { $chg++ } }',
      '}',
      '"$keep/$nin/$chg/$nout"',
      '$a.Dispose();$b.Dispose()',
    ].join('\n');
    const resInv = execFileSync('powershell', ['-NoProfile', '-Command', psInv], {
      encoding: 'utf8',
      // 羽化带会吃掉笔迹边缘往里 0.4×外扩 + 约 1.5σ（≈160px），判"保住"要避开这一圈，
      // 与正向那次用 SAFE_R 避开外扩带是同一个口径
      env: { ...process.env, P1: f1, P2: f3, CX: String(INK.cx), CY: String(INK.cy), RR: String(INK.r), RIN: String(INK.r - 160) },
    }).trim().split(/\r?\n/).pop();
    const [keep, nin, chg, nout] = resInv.split('/').map(Number);
    check(`反向：圈住的主体逐字节没动（取样 ${nin} 点，半径 ${INK.r - 160} 内）`, nin > 30 && keep === nin, resInv);
    // 远处应当几乎全换成模型给的内容；配色校正会让个别点碰巧相同，所以按 95% 判
    check(`反向：主体之外整幅换掉（取样 ${nout} 点）`, nout > 60 && chg / nout > 0.95, resInv);

    // 一笔没涂 = 整幅重绘：这时不造一张全透明遮罩发出去，而是干脆不带 mask 这个 part
    const b64 = 'data:image/png;base64,' + mask.toString('base64');
    await req(srv.base, 'POST', `/api/images/${iid}/mask`, {});       // 清空遮罩文件
    const qAll = await req(srv.base, 'POST', '/api/cloud/queue', { image_ids: [iid], settings: { prompt: 'INVERT 整幅重绘', invert: true } });
    const ridAll = (qAll.body.results || [])[0]?.result_id;
    const stAll = await waitRows(srv.base, [ridAll], ['done', 'error']);
    const allHit = mock.hits[mock.hits.length - 1];
    check('反向且没涂也允许提交', stAll[0] === 'done', JSON.stringify(stAll) + ' ' + JSON.stringify(qAll.body).slice(0, 120));
    check('没涂时不带 mask part', !!allHit && allHit.hasMask === false, JSON.stringify(allHit));
    await req(srv.base, 'POST', `/api/images/${iid}/mask`, { b64 });  // 后面的用例还要这张遮罩
  }

  // ---- 整图重绘（原图直发）：不看遮罩、不发 mask、回来不缝合 ----
  const qFull = await req(srv.base, 'POST', '/api/cloud/queue', { image_ids: [iid], settings: { prompt: 'FULL 整张重绘', full: true, edge: 1024 } });
  const ridFull = (qFull.body.results || [])[0]?.result_id;
  const stFull = await waitRows(srv.base, [ridFull], ['done', 'error']);
  const fullHit = mock.hits[mock.hits.length - 1];
  check('整张重绘跑到 done', stFull[0] === 'done', JSON.stringify(stFull) + ' ' + JSON.stringify(qFull.body).slice(0, 140));
  check('整张重绘连遮罩都不发（这张明明涂着）', !!fullHit && fullHit.hasMask === false, JSON.stringify(fullHit && { size: fullHit.size, hasMask: fullHit.hasMask }));
  check('整张重绘发的是整图折到 1024 档', Math.max(...String(fullHit && fullHit.size).split('x').map(Number)) === 1024, JSON.stringify(fullHit && fullHit.size));
  const fullRow = (await req(srv.base, 'GET', `/api/results/${ridFull}`)).body;
  check('整图成图没有裁切/遮罩那两张诊断图', fullRow.crop_url === null && fullRow.maskoverlay_url === null, JSON.stringify({ c: fullRow.crop_url, m: fullRow.maskoverlay_url }));
  const fullBuf = (await req(srv.base, 'GET', fullRow.final_url)).body;
  check('成图按折后的尺寸存，不放大回原图', pngSize(fullBuf).w < W && pngSize(fullBuf).h < H, JSON.stringify(pngSize(fullBuf)));
  check('这一行的参数快照标了 full', JSON.parse(fullRow.settings_json || '{}').full === true, fullRow.settings_json);
  const localFull = await req(srv.base, 'POST', '/api/run', { image_ids: [iid], settings: { prompt: 'x', steps: 20, cfg: 3, full: true } });
  const lf = (localFull.body.results || [])[0] || {};
  check('本机那条遇到 full 明确拒绝而不是偷跑局部重绘', lf.skipped === true && /只走云端/.test(String(lf.reason)), JSON.stringify(lf));

  // ---- 画布笔迹快照：每一步各存一份，取回要逐字节回到当时 ----
  const cv = await req(srv.base, 'POST', '/api/canvas/create', { name: '快照自检', w: 1024, h: 1024 });
  const cid = cv.body.image_id;
  const ink = (px) => png(1024, 1024, (x, y) => px(x, y) ? [31, 30, 28, 255] : [0, 0, 0, 0]);
  const ink1 = ink((x, y) => x > 100 && x < 160 && y > 200 && y < 800);
  const ink2 = ink((x, y) => y > 500 && y < 560 && x > 100 && x < 900);
  const put = async b => req(srv.base, 'POST', `/api/canvas/${cid}/sketch`, { b64: 'data:image/png;base64,' + b.toString('base64') });
  await put(ink1);
  const g1 = await req(srv.base, 'POST', `/api/canvas/${cid}/generate`, { settings: { prompt: '第一版的提示词' } });
  const rid1 = g1.body.result_id;
  await waitRows(srv.base, [rid1], ['done', 'error']);
  const row1 = (await req(srv.base, 'GET', `/api/results/${rid1}`)).body;
  check('画布这一版留下了线稿快照', /_sketch\.png$/.test(String(row1.sketch_url || '')), JSON.stringify(row1.sketch_url));
  const snap1 = (await req(srv.base, 'GET', row1.sketch_url)).body;
  check('快照是当时那份画稿的原件（逐字节，不重编码）', Buffer.compare(snap1, ink1) === 0, `${snap1.length} vs ${ink1.length}`);
  await put(ink2);
  const g2 = await req(srv.base, 'POST', `/api/canvas/${cid}/generate`, { settings: { prompt: '第二版的提示词' }, rerun_of: rid1 });
  const rid2 = g2.body.result_id;
  await waitRows(srv.base, [rid2], ['done', 'error']);
  const row2 = (await req(srv.base, 'GET', `/api/results/${rid2}`)).body;
  const snap2 = (await req(srv.base, 'GET', row2.sketch_url)).body;
  const sent2 = mock.hits[mock.hits.length - 1];
  check('每一步各存一份，两版互不覆盖', Buffer.compare(snap1, snap2) !== 0 && Buffer.compare(snap2, ink2) === 0,
    JSON.stringify({ s1: snap1.length, s2: snap2.length }));

  /* 排在队列里的时候接着画两笔：发出去的必须还是**这一行提交那一刻**那份快照，而不是此刻的画稿。
     并发收到 1、用一个慢回的任务占住唯一的坑，g3 才会真的停在 queued——
     不然 generate 一返回它就被叫走，改了画稿也测不到这一条。
     比的是"发出去的字节"而不是"存下来的字节"：后者以前就断言过，正是这条漏掉了才让 t0/t1 错位溜过去。 */
  await req(srv.base, 'POST', '/api/cloud', { concurrency: '1' });
  const qHold = await req(srv.base, 'POST', '/api/cloud/queue', { image_ids: [iid], settings: { prompt: 'HOLD 占位' } });
  const ridHold = (qHold.body.results || [])[0]?.result_id;
  const g3 = await req(srv.base, 'POST', `/api/canvas/${cid}/generate`, { settings: { prompt: '排队中改了画稿' } });
  const rid3 = g3.body.result_id;
  const ink3 = ink((x, y) => x > 700 && x < 760 && y > 100 && y < 900);
  await put(ink3);
  const stQ = await waitRows(srv.base, [ridHold, rid3], ['done', 'error']);
  const sent3 = mock.hits[mock.hits.length - 1];
  check('排队期间改的画稿不会倒灌进这一枪', stQ.every(s => s === 'done') && !!sent2 && !!sent3
    && Buffer.compare(sent2.image, sent3.image) === 0,
    JSON.stringify({ st: stQ, s2: sent2 && sent2.image.length, s3: sent3 && sent3.image.length }));
  // 对照：笔迹真换了，发出去的就得跟着变——不然上面那条"相等"可能只是因为两次都没送出东西
  const g4 = await req(srv.base, 'POST', `/api/canvas/${cid}/generate`, { settings: { prompt: '画稿已经是第三版了' } });
  const rid4 = g4.body.result_id;
  await waitRows(srv.base, [rid4], ['done', 'error']);
  const sent4 = mock.hits[mock.hits.length - 1];
  check('换了笔迹之后发出去的确实跟着变', !!sent4 && Buffer.compare(sent3.image, sent4.image) !== 0,
    JSON.stringify({ s3: sent3 && sent3.image.length, s4: sent4 && sent4.image.length }));
  await req(srv.base, 'POST', '/api/cloud', { concurrency: '2' });

  /* ---- 参考图：槽位 → 这一行自己的快照 → 发出去的每一个 part ----
     提交后立刻把槽位清空：这一版发出去的还是当次那两张，才说明"参考了哪几张"是行上的事实而不是槽位的事实 */
  const b64of = buf => 'data:image/png;base64,' + buf.toString('base64');
  const refA = png(1024, 1024, (x) => (x > 512 ? [200, 30, 26, 255] : [0, 0, 0, 0]));     // 半张透明：该被拍到白底上
  const refB = png(1024, 1024, () => [24, 56, 168, 255]);
  const addR = await req(srv.base, 'POST', `/api/canvas/${cid}/refs`, { files: [{ b64: b64of(refA) }, { b64: b64of(refB) }] });
  check('两张参考图挂进槽位', addR.status === 200 && (addR.body.refs || []).length === 2, JSON.stringify(addR.body).slice(0, 200));
  const g5 = await req(srv.base, 'POST', `/api/canvas/${cid}/generate`, { settings: { prompt: '带两张参考图这一版' } });
  const rid5 = g5.body.result_id;
  const cleared = await req(srv.base, 'PUT', `/api/canvas/${cid}/refs`, { paths: [] });
  await waitRows(srv.base, [rid5], ['done', 'error']);
  const row5 = (await req(srv.base, 'GET', `/api/results/${rid5}`)).body;
  const snap5 = await Promise.all((row5.refs || []).map(async r => (await req(srv.base, 'GET', r.url)).body));
  const hit5 = mock.hits[mock.hits.length - 1];
  check('清空槽位不影响这一版的参考图', cleared.status === 200 && (cleared.body.refs || []).length === 0, JSON.stringify(cleared.body).slice(0, 120));
  check('这一版行上记着两张自己的参考快照', (row5.refs || []).length === 2
    && row5.refs.every(r => /_ref\d\.png$/.test(String(r.url)) && r.dead === false), JSON.stringify(row5.refs).slice(0, 220));
  check('发出去的 part = 画稿 + 这两张快照（逐字节）', !!hit5 && hit5.images.length === 3
    && Buffer.compare(hit5.images[1], snap5[0]) === 0 && Buffer.compare(hit5.images[2], snap5[1]) === 0,
    JSON.stringify({ parts: hit5 && hit5.images.length, refs: snap5.map(b => b.length) }));

  /* 画稿一笔没涂 + 挂了参考图：不发那张全白的纸，这一枪就是"按参考图与提示词生成" */
  await put(ink(() => false));
  await req(srv.base, 'POST', `/api/canvas/${cid}/refs`, { files: [{ b64: b64of(refB) }] });
  const g6 = await req(srv.base, 'POST', `/api/canvas/${cid}/generate`, { settings: { prompt: '只有参考图这一版' } });
  const rid6 = g6.body.result_id;
  await waitRows(srv.base, [rid6], ['done', 'error']);
  const row6 = (await req(srv.base, 'GET', `/api/results/${rid6}`)).body;
  const snap6 = (await req(srv.base, 'GET', row6.refs[0].url)).body;
  const hit6 = mock.hits[mock.hits.length - 1];
  check('空白画稿 + 参考图：只发参考图，不发那张全白的纸', (row6.refs || []).length === 1
    && !!hit6 && hit6.images.length === 1 && Buffer.compare(hit6.images[0], snap6) === 0,
    JSON.stringify({ parts: hit6 && hit6.images.length, refs: (row6.refs || []).length }));

  await put(ink(() => false));                     // 再画"干净"，屏幕上已经不是第一版
  const took = await req(srv.base, 'POST', `/api/canvas/${cid}/use-sketch`, { result_id: rid1 });
  const cur0 = (await req(srv.base, 'GET', `/api/canvas/${cid}`)).body;
  const back1 = (await req(srv.base, 'GET', cur0.sketch_url)).body;
  check('取回第一版画稿成功', took.status === 200 && took.body.ok === true, JSON.stringify(took.body));
  check('取回后画稿逐字节回到第一版', Buffer.compare(back1, ink1) === 0, `${back1.length} vs ${ink1.length}`);
  const wrong = await req(srv.base, 'POST', `/api/canvas/${cid}/use-sketch`, { result_id: rid1 - 1000 });
  check('取回不认的记录不会覆盖画稿', wrong.status >= 400 || (wrong.body && wrong.body.error), JSON.stringify(wrong.body).slice(0, 120));
  const snapRel = String(row2.sketch_url).replace(/^\/file\//, '');
  check('快照文件确实在盘上', fs.existsSync(path.join(data, snapRel)), snapRel);
  await req(srv.base, 'DELETE', `/api/results/${rid2}`);
  check('删这条记录连带删掉它的快照', !fs.existsSync(path.join(data, snapRel)), '快照还留在盘上');
  check('删一行不影响另一行的快照', fs.existsSync(path.join(data, String(row1.sketch_url).replace(/^\/file\//, ''))), row1.sketch_url);

  // ---- 派生谱系：fork 写父子；老库那种"只有名字里有派生号"的行靠重启回填 ----
  const fork = await req(srv.base, 'POST', `/api/results/${rows[0]}/fork`, {});
  const kidId = fork.body.image_id;
  check('另存为新图建出了子图', !!kidId && fork.body.derived_from === iid, JSON.stringify(fork.body).slice(0, 160));
  const kids = (await req(srv.base, 'GET', `/api/images/${iid}/derived`)).body;
  check('派生列表按父子关系查得到', (kids.images || []).some(k => k.id === kidId), JSON.stringify(kids).slice(0, 160));
  const legacyName = `老图 派生${rows[0]}.png`;
  await req(srv.base, 'POST', `/api/projects/${proj.body.id}/images`, { files: [{ name: legacyName, b64: 'data:image/png;base64,' + png(2, 2, () => [9, 9, 9, 255]).toString('base64'), w: 2, h: 2 }] });
  const pj2 = (await req(srv.base, 'GET', `/api/projects/${proj.body.id}`)).body;
  const prow = (pj2.images || []).find(x => x.id === iid) || {};
  const krow = (pj2.images || []).find(x => x.id === kidId) || {};
  const lrow = (pj2.images || []).find(x => x.name === legacyName) || {};
  check('父图角标带得出派生计数', prow.derived_count === 1, JSON.stringify(prow.derived_count));
  check('子图行认得自己的父图与来源结果', krow.derived_from === iid && krow.derived_result === rows[0], JSON.stringify({ d: krow.derived_from, r: krow.derived_result }));
  check('刚导入的行没有父子（只有 fork 与老名字才有）', lrow.derived_from == null, JSON.stringify(lrow.derived_from));

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
  // 老库的派生行是启动时按文件名回填的：名字里那个号必须能反查回来源结果的图
  const pj3 = (await req(srv.base, 'GET', `/api/projects/${proj.body.id}`)).body;
  const lrow2 = (pj3.images || []).find(x => x.name === legacyName) || {};
  check('重启后老派生行按文件名回填上了父图', lrow2.derived_from === iid, JSON.stringify({ n: lrow2.name, d: lrow2.derived_from }));
  const kids2 = (await req(srv.base, 'GET', `/api/images/${iid}/derived`)).body;
  check('回填的子图也进得了派生列表', (kids2.images || []).length === 2, JSON.stringify((kids2.images || []).map(k => k.id)));
  // 删父图：子图作为独立成品要留着，但来源引用得清掉（否则计数指向空 id）
  const del = await req(srv.base, 'DELETE', `/api/images/${iid}`);
  const kd3 = await req(srv.base, 'GET', `/api/images/${iid}/derived`);
  const pj4 = (await req(srv.base, 'GET', `/api/projects/${proj.body.id}`)).body;
  const still = (pj4.images || []).find(x => x.id === kidId);
  check('删父图不连带删掉用户另存出去的子图', del.status === 200 && !!still, JSON.stringify({ s: del.status, still: !!still }));
  check('父图没了，子图的来源引用被清掉', kd3.status === 200 && (kd3.body.images || []).length === 0 && still.derived_from == null,
    JSON.stringify({ ks: kd3.status, n: (kd3.body.images || []).length, d: still.derived_from }));

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
