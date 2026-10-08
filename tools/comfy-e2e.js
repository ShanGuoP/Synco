// M7 第二步端到端：角色映射接管本机提交。假 ComfyUI 收在 127.0.0.1 随机端口，
// 它把 /prompt 收到的计算图整份记下来，所以我们能断言"提交出去的就是你那张图"，
// 而不只是"接口没炸"。全程用临时 DATA，不碰你正在跑的 7861，也不写真实 data/。
//
//   node tools/comfy-e2e.js            # 自己起假 ComfyUI + 隔离实例，跑完自己收
//   node tools/comfy-e2e.js --keep     # 保留临时 DATA 便于复查
'use strict';
const { spawn } = require('child_process');
const fs = require('fs');
const os = require('os');
const http = require('http');
const path = require('path');

const ROOT = path.join(__dirname, '..');
const BIN = path.join(ROOT, 'target', 'debug', 'synco.exe');
const PNG1 = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==',
  'base64'
);
const B64 = PNG1.toString('base64');
const fails = [];
const ok = (name, cond, detail) => {
  console.log(`${cond ? '  ✓' : '  ✗'} ${name}${cond ? '' : '：' + String(detail).slice(0, 320)}`);
  if (!cond) fails.push(name);
};

// ---- 用户那张图：节点号刻意与内置图那套（1/2/…/17/19/20）不同 --------------------
// 51 = 缝合结果存盘（必需），19/20 = 裁切区与遮罩预览（可选）
function userGraph(extra) {
  const g = {
    '21': { class_type: 'LoadImage', inputs: { image: '占位.png' } },
    '22': { class_type: 'LoadImageMask', inputs: { image: '占位m.png', channel: 'red' } },
    '30': { class_type: 'UNETLoader', inputs: { unet_name: 'qwen_bf16.safetensors', weight_dtype: 'default' } },
    '31': { class_type: 'LoraLoaderModelOnly', inputs: { model: ['30', 0], lora_name: 'A.safetensors', strength_model: 0.9 } },
    '32': { class_type: 'LoraLoaderModelOnly', inputs: { model: ['31', 0], lora_name: 'B.safetensors', strength_model: 0.5 } },
    '36': { class_type: 'CLIPLoader', inputs: { clip_name: 'clip.safetensors', type: 'qwen' } },
    '37': { class_type: 'VAELoader', inputs: { vae_name: 'vae.safetensors' } },
    '40': { class_type: 'InpaintCropImproved', inputs: { image: ['21', 0], mask: ['22', 0], mask_expand_pixels: 64, device_mode: 'cpu (compatible)' } },
    '41': { class_type: 'TextEncodeQwenImage21', inputs: { clip: ['36', 0], vae: ['37', 0], prompt: '文件里的正向', negative_prompt: '文件里的负面', 'images.image_1': ['40', 1] } },
    '42': { class_type: 'VAEEncode', inputs: { pixels: ['40', 1], vae: ['37', 0] } },
    '43': { class_type: 'KSampler', inputs: { model: ['32', 0], positive: ['41', 0], negative: ['41', 1], latent_image: ['42', 0], seed: 7, steps: 18, cfg: 2.5, sampler_name: 'euler', scheduler: 'simple', denoise: 1 } },
    '45': { class_type: 'VAEDecode', inputs: { samples: ['43', 0], vae: ['37', 0] } },
    '50': { class_type: 'InpaintStitchImproved', inputs: { stitcher: ['40', 0], inpainted_image: ['45', 0] } },
    '51': { class_type: 'SaveImage', inputs: { images: ['50', 0], filename_prefix: 'Mine' } },
    '18': { class_type: 'DrawMaskOnImage', inputs: { image: ['40', 1], mask: ['40', 2], color: '0, 0, 255' } },
    '19': { class_type: 'PreviewImage', inputs: { images: ['40', 1] } },
    '20': { class_type: 'PreviewImage', inputs: { images: ['18', 0] } },
  };
  if (extra === 'nostitch') delete g['50'];
  if (extra === 'lean') { delete g['18']; delete g['19']; delete g['20']; }
  return g;
}

// ---- 假 ComfyUI：记下图、认任务、按节点号回吐产物 -------------------------------
function startMock() {
  const hits = [];           // 每次 /prompt 收到的计算图
  let n = 0;
  const srv = http.createServer((req, res) => {
    const bufs = [];
    req.on('data', b => bufs.push(b));
    req.on('end', () => {
      const body = Buffer.concat(bufs);
      const url = req.url || '/';
      const json = (code, val) => { res.writeHead(code, { 'content-type': 'application/json' }); res.end(JSON.stringify(val)); };
      if (url === '/system_stats') return json(200, { devices: [], version: 'mock' });
      if (url === '/queue' && req.method === 'GET') return json(200, { queue_running: [], queue_pending: [] });
      if (url === '/upload/image') {
        const fn = /filename="([^"]+)"/.exec(body.toString('latin1'));
        return json(200, { name: fn ? fn[1] : 'x.png', subfolder: 'mocksub', type: 'input' });
      }
      if (url === '/prompt') {
        const g = JSON.parse(body.toString('utf8')).prompt;
        n += 1;
        hits.push({ pid: `mock-${n}`, graph: g });
        // 拒收不认识的图：把"缺节点"这类问题留在这一层，好让上层断言有意义
        for (const [id, node] of Object.entries(g)) {
          for (const [k, v] of Object.entries(node.inputs || {})) {
            if (Array.isArray(v) && !g[String(v[0])]) return json(400, { error: { node_id: id, message: `节点 ${id} 的 ${k} 指向不存在的 ${v[0]}` } });
          }
        }
        return json(200, { prompt_id: `mock-${n}`, number: n });
      }
      const h = /^\/history\/(.+)$/.exec(url);
      if (h) {
        const hit = hits.find(x => x.pid === h[1]);
        if (!hit) return json(200, {});
        const outputs = {};
        for (const [id, node] of Object.entries(hit.graph)) {
          if (node.class_type === 'SaveImage' || node.class_type === 'PreviewImage') {
            outputs[id] = { images: [{ filename: `out_${id}.png`, subfolder: 'gens', type: 'output' }] };
          }
        }
        return json(200, { [h[1]]: { outputs, status: { status_str: 'success', completed: true, messages: [['execution_success', {}]] } } });
      }
      if (/^\/view\?/.test(url)) { res.writeHead(200, { 'content-type': 'image/png' }); return res.end(PNG1); }
      json(404, { error: `假 ComfyUI 不认 ${req.method} ${url}` });
    });
  });
  return new Promise(resolve => {
    srv.listen(0, '127.0.0.1', () => resolve({ url: `http://127.0.0.1:${srv.address().port}`, hits, close: () => srv.close() }));
  });
}

async function boot(dataDir) {
  const proc = spawn(BIN, [], { cwd: ROOT, env: { ...process.env, SYNCO_DATA: dataDir, SYNCO_PORT: '0' }, stdio: ['ignore', 'pipe', 'pipe'] });
  const log = [];
  proc.stdout.on('data', b => log.push(b.toString()));
  proc.stderr.on('data', b => log.push(b.toString()));
  const t0 = Date.now();
  let port = 0;
  // 端口不再往盘上落一份：从服务自己打的 SYNCO_URL 里读，就是它实际绑成的那个
  while (!port) {
    const m = /SYNCO_URL=http:\/\/127\.0\.0\.1:(\d+)/.exec(log.join(''));
    if (m) port = Number(m[1]);
    if (proc.exitCode !== null) throw new Error(`服务提前退出 code=${proc.exitCode}：\n${log.join('')}`);
    if (Date.now() - t0 > 20000) throw new Error(`等 SYNCO_URL 超时：\n${log.join('')}`);
    await new Promise(r => setTimeout(r, 120));
  }
  const base = `http://127.0.0.1:${port}`;
  const t1 = Date.now();
  while (Date.now() - t1 < 8000) {
    try { const r = await fetch(base + '/api/projects', { signal: AbortSignal.timeout(1500) }); if (r.ok) return { proc, base, log }; } catch {}
    await new Promise(r => setTimeout(r, 150));
  }
  throw new Error(`服务在 ${base} 上没答话：\n${log.join('')}`);
}

async function req(base, method, p, body) {
  const opt = { method, signal: AbortSignal.timeout(15000) };
  if (body !== undefined) { opt.body = JSON.stringify(body); opt.headers = { 'content-type': 'application/json' }; }
  const r = await fetch(base + p, opt);
  const val = await r.json().catch(() => null);
  return { status: r.status, body: val };
}

/** 轮询到落定：读一次就推进一次状态机，与前端用的是同一条路 */
async function settle(base, rid) {
  for (let i = 0; i < 80; i++) {
    const r = await req(base, 'GET', `/api/results/${rid}`);
    const st = r.body && r.body.status;
    if (st === 'done' || st === 'error') return r.body;
    await new Promise(x => setTimeout(x, 120));
  }
  return { status: 'timeout' };
}

async function submit(base, iid, settings) {
  const r = await req(base, 'POST', '/api/run', { image_ids: [iid], settings });
  const row = (r.body.results || [])[0] || {};
  return row;
}

async function main() {
  if (!fs.existsSync(BIN)) throw new Error(`先 cargo build -p synco-server（找不到 ${BIN}）`);
  const keep = process.argv.includes('--keep');
  const dataDir = path.resolve(os.tmpdir(), `synco-comfy-${Date.now()}`);
  // 与另外两条自检同一条纪律：临时 DATA 绝不能落在仓库里，否则写的就是他真实的 data/
  if (dataDir === path.resolve(ROOT) || dataDir.startsWith(path.resolve(ROOT) + path.sep)) {
    throw new Error(`临时 DATA 落在仓库里了：${dataDir}（TMPDIR=${os.tmpdir()}）`);
  }
  fs.mkdirSync(dataDir, { recursive: true });
  const mock = await startMock();
  const { proc, base } = await boot(dataDir);
  console.log(`Rust ${base.replace(/^http:\/\/127\.0\.0\.1:/, '')} @ ${dataDir}\n假 ComfyUI ${mock.url}\n`);

  // 后端指向假 ComfyUI
  await req(base, 'POST', '/api/backends', { url: mock.url, label: '假 ComfyUI' });
  const sel = await req(base, 'POST', '/api/backends/select', { url: mock.url });
  ok('后端切到假 ComfyUI', sel.status === 200 && sel.body.active === mock.url, JSON.stringify(sel.body));

  // 建一张图 + 遮罩，本机那一路要用
  const proj = await req(base, 'POST', '/api/projects', { name: '接管自检', files: [{ name: 'a.png', b64: B64, w: 2, h: 2 }] });
  const iid = proj.body.image_ids[0];
  await req(base, 'POST', `/api/images/${iid}/mask`, { b64: B64 });
  const settings = {
    prompt: '把妆容修干净', negative: '', steps: 22, cfg: 4, seed: 10, randomSeed: false,
    loras: [{ name: 'A.safetensors', strength: 0.4, enabled: false }, { name: 'B.safetensors', strength: 0.8, enabled: true }],
  };

  // ---------- 1. 角色齐 → 用你那张图提交，并按你的节点号读回 ----------
  const wfPath = path.join(dataDir, 'wf-user.json').replace(/\\/g, '/');
  fs.writeFileSync(wfPath, JSON.stringify(userGraph()));
  const setWf = await req(base, 'POST', '/api/settings/workflow', { path: wfPath });
  ok('API 导出被认出来', setWf.body.cfg_source === 'workflow', JSON.stringify(setWf.body));
  const roles0 = await req(base, 'GET', '/api/workflow/roles');
  ok('角色表能读到且校验通过', roles0.status === 200 && roles0.body.can_takeover === true, JSON.stringify(roles0.body).slice(0, 260));
  ok('自动认出成图那一路', roles0.body.effective.out_final === '51' && roles0.body.effective.out_crop === '19' && roles0.body.effective.out_overlay === '20',
    JSON.stringify(roles0.body.effective));
  ok('节点清单给全了', (roles0.body.nodes || []).length === 17, `${(roles0.body.nodes || []).length} 个`);

  const run1 = await submit(base, iid, settings);
  ok('提交成功建了行', !!run1.result_id, JSON.stringify(run1));
  const hit1 = mock.hits[0] || {};
  const g1 = hit1.graph || {};
  ok('提交的是你那张图（不是内置那套节点号）', !!g1['51'] && !!g1['40'] && !g1['17'] && !g1['10'], JSON.stringify(Object.keys(g1)));
  ok('照片送进了你的加载图节点', /^mocksub\/p/.test(String(g1['21'] && g1['21'].inputs.image)), String(g1['21'] && g1['21'].inputs.image));
  ok('遮罩送进了你的加载遮罩节点', /^mocksub\/p.*_m_/.test(String(g1['22'] && g1['22'].inputs.image)), String(g1['22'] && g1['22'].inputs.image));
  ok('面板提示词覆盖了你文件里那句', g1['41'] && g1['41'].inputs.prompt === '把妆容修干净', JSON.stringify(g1['41'] && g1['41'].inputs));
  ok('负面词为空时不抹你文件里那句', g1['41'] && g1['41'].inputs.negative_prompt === '文件里的负面', JSON.stringify(g1['41'] && g1['41'].inputs.negative_prompt));
  ok('种子/步数/CFG 落到你的采样器', g1['43'] && g1['43'].inputs.seed === 10 + iid && g1['43'].inputs.steps === 22 && g1['43'].inputs.cfg === 4,
    JSON.stringify(g1['43'] && g1['43'].inputs));
  ok('采样器选型按你文件里那样跑', g1['43'] && g1['43'].inputs.sampler_name === 'euler' && g1['43'].inputs.scheduler === 'simple', '');
  ok('裁切参数没被内置值覆盖', g1['40'] && g1['40'].inputs.mask_expand_pixels === 64 && g1['40'].inputs.device_mode === 'cpu (compatible)', JSON.stringify(g1['40'] && g1['40'].inputs));
  ok('关掉的 LoRA 节点被摘掉', !g1['31'], JSON.stringify(g1['31']));
  ok('剩下的 LoRA 接回 unet', g1['32'] && JSON.stringify(g1['32'].inputs.model) === '["30",0]', JSON.stringify(g1['32'] && g1['32'].inputs.model));
  ok('开着的 LoRA 用了面板强度', g1['32'] && g1['32'].inputs.strength_model === 0.8, JSON.stringify(g1['32'] && g1['32'].inputs.strength_model));

  const r1 = await settle(base, run1.result_id);
  ok('这一张跑完了', r1.status === 'done', JSON.stringify({ status: r1.status, error: r1.error }));
  ok('成图从你的 51 号节点读回来', /^\/file\/.+_final\.png$/.test(String(r1.final_url)), String(r1.final_url));
  ok('裁切区与遮罩图分别从 19/20 读回来', /_crop\.png$/.test(String(r1.crop_url)) && /_mask\.png$/.test(String(r1.maskoverlay_url)),
    JSON.stringify({ crop: r1.crop_url, mask: r1.maskoverlay_url }));
  const snap1 = JSON.parse(r1.settings_json || '{}');
  ok('这一行记住了用谁的图', snap1.graph_source === 'workflow', JSON.stringify(snap1.graph_source));
  ok('这一行记住了输出节点号', snap1.workflow_out && snap1.workflow_out.final === '51' && snap1.workflow_out.crop === '19' && snap1.workflow_out.maskoverlay === '20',
    JSON.stringify(snap1.workflow_out));
  const files1 = fs.readdirSync(path.join(dataDir, 'projects', String(proj.body.id))).filter(f => /^r\d+_.*_(final|crop|mask)\.png$/.test(f));
  ok('三张都真落盘了', files1.length >= 3, files1.join(','));

  // ---------- 2. 可选输出缺席：不该把一张成功成图判死 ----------
  fs.writeFileSync(wfPath, JSON.stringify(userGraph('lean')));
  await req(base, 'POST', '/api/settings/workflow', { path: wfPath });
  const roles2 = await req(base, 'GET', '/api/workflow/roles');
  ok('没有预览节点的图也能接管（只缺可选角色）', roles2.body.can_takeover === true && !roles2.body.effective.out_crop, JSON.stringify(roles2.body.errors));
  const run2 = await submit(base, iid, settings);
  const r2 = await settle(base, run2.result_id);
  ok('少了两路输出照样算成', r2.status === 'done', JSON.stringify({ status: r2.status, error: r2.error }));
  ok('裁切区与遮罩图回 null 而不是空 URL', r2.crop_url === null && r2.maskoverlay_url === null, JSON.stringify({ crop: r2.crop_url, mask: r2.maskoverlay_url }));
  ok('这一行只记了成图节点', JSON.stringify(JSON.parse(r2.settings_json).workflow_out) === '{"final":"51"}', r2.settings_json);

  // ---------- 3. 必需角色缺位：明确拒绝，不回退内置图 ----------
  fs.writeFileSync(wfPath, JSON.stringify(userGraph('nostitch')));
  await req(base, 'POST', '/api/settings/workflow', { path: wfPath });
  const roles3 = await req(base, 'GET', '/api/workflow/roles');
  ok('缺缝合就说清缺了谁', roles3.body.can_takeover === false && String(roles3.body.errors.join('')).includes('缝合'), JSON.stringify(roles3.body.errors));
  const hitsBefore = mock.hits.length;
  const run3 = await submit(base, iid, settings);
  ok('提交被拒而不是静默改用内置图', run3.skipped === true && String(run3.reason).includes('不能接管提交') && String(run3.reason).includes('缝合'), JSON.stringify(run3));
  ok('拒掉之后没有偷偷发给 ComfyUI', mock.hits.length === hitsBefore, `${mock.hits.length} vs ${hitsBefore}`);

  // ---------- 4. UI 导出：走内置图，但把原因写明 ----------
  const litePath = path.join(dataDir, 'wf-lite.json').replace(/\\/g, '/');
  fs.writeFileSync(litePath, JSON.stringify({ nodes: [{ id: 3, type: 'UNETLoader', mode: 0, widgets_values: ['u.safetensors', 'default'] }], links: [] }));
  await req(base, 'POST', '/api/settings/workflow', { path: litePath });
  const roles4 = await req(base, 'GET', '/api/workflow/roles');
  ok('UI 导出认当不了计算图', roles4.body.is_api === false && String(roles4.body.reason).includes('UI 导出'), JSON.stringify(roles4.body.reason));
  const run4 = await submit(base, iid, settings);
  const r4 = await settle(base, run4.result_id);
  const g4 = (mock.hits[mock.hits.length - 1] || {}).graph || {};
  ok('退回内置图时提交的是内置那套', !!g4['17'] && !!g4['10'], JSON.stringify(Object.keys(g4)).slice(0, 160));
  ok('这一行标了内置图', r4.status === 'done' && JSON.parse(r4.settings_json).graph_source === 'builtin', JSON.stringify({ s: r4.status, g: JSON.parse(r4.settings_json || '{}').graph_source }));

  // ---------- 5. 手指覆盖：错类名不收，改过的能覆盖自动认出 ----------
  fs.writeFileSync(wfPath, JSON.stringify(userGraph()));
  await req(base, 'POST', '/api/settings/workflow', { path: wfPath });
  const badPut = await req(base, 'POST', '/api/workflow/roles', { roles: { ksampler: '21' } });
  ok('把采样器指到加载图上被拒', badPut.status === 200 && (badPut.body.rejected || []).length === 1 && String(badPut.body.rejected[0]).includes('KSampler'),
    JSON.stringify(badPut.body.rejected));
  const goodPut = await req(base, 'POST', '/api/workflow/roles', { roles: { out_final: '19' } });
  ok('指到别的类也被拒', (goodPut.body.rejected || []).length === 1, JSON.stringify(goodPut.body.rejected));
  // 同类的另一个 SaveImage 图里没有，那就用"节点不存在"这条：手输一个 999
  const ghost = await req(base, 'POST', '/api/workflow/roles', { roles: { out_final: '999' } });
  ok('指向图里不存在的节点被拒', (ghost.body.rejected || []).length === 1 && String(ghost.body.rejected[0]).includes('999'), JSON.stringify(ghost.body.rejected));
  const cleared = await req(base, 'POST', '/api/workflow/roles', { roles: { out_final: '' } });
  ok('清空等于交回自动认出', cleared.body.saved && Object.keys(cleared.body.saved).length === 0 && cleared.body.effective.out_final === '51',
    JSON.stringify({ saved: cleared.body.saved, eff: cleared.body.effective.out_final }));
  const rerun = await submit(base, iid, settings);
  ok('角色表改完还能提交', !!rerun.result_id, JSON.stringify(rerun));

  proc.kill();
  mock.close();
  // 进程还在退的时候 Windows 锁着目录，删不掉是常事：不能因为它把一次全绿的跑法报成退出码 1
  if (!keep) {
    await new Promise(r => setTimeout(r, 600));
    try { fs.rmSync(dataDir, { recursive: true, force: true }); } catch { console.log(`（临时目录留着了：${dataDir}）`); }
  }
  console.log(`\n${fails.length ? '✗ 失败 ' + fails.length + ' 条：' + fails.join('、') : '✓ 全绿'}${keep ? `（DATA 留在 ${dataDir}）` : ''}`);
  process.exit(fails.length ? 1 : 0);
}

main().catch(e => { console.error('✗ 跑挂了：', e.message); process.exit(1); });
