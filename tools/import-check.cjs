'use strict';
// 隔离验证图片接收：旧体积/尺寸门槛、真实内容校验及前端 RAW 筛除。
const fs = require('node:fs'), path = require('node:path'), os = require('node:os');
const assert = require('node:assert/strict'), crypto = require('node:crypto'), vm = require('node:vm');
const { spawn } = require('node:child_process');
const root = path.resolve(__dirname, '..');
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
async function until(fn) {
  const start = Date.now();
  while (Date.now() - start < 60000) { const value = await fn(); if (value) return value; await pause(100); }
  throw Error('Import probe timed out');
}
async function main() {
  const src = fs.readFileSync(path.join(root, 'public/js/core/api.js'), 'utf8');
  const sandbox = { t: k => k };
  vm.createContext(sandbox);
  vm.runInContext(src.slice(src.indexOf('export const IMAGE_ACCEPT'), src.indexOf('export async function importPhotos')).replaceAll('export const', 'const') + '\nthis.accept = isImageFile; this.groups = batches;', sandbox);
  assert(sandbox.accept({ name: 'large.JPG', type: '', size: 1024 ** 3 }));
  assert(!sandbox.accept({ name: 'source.CR3', type: 'image/x-canon-cr3' }));
  assert(!sandbox.accept({ name: 'source.dng', type: 'image/jpeg' }));
  assert(!sandbox.accept({ name: 'source.avif', type: 'image/avif' }));
  assert.equal(sandbox.groups([{ size: 1024 ** 3 }, { size: 1 }]).length, 2);
  for (const lang of ['zh', 'en']) {
    const locale = JSON.parse(fs.readFileSync(path.join(root, `public/locales/${lang}.json`)));
    assert.equal(locale.hist.all, lang === 'zh' ? '全部' : 'All');
    assert.equal(locale.hist.title, lang === 'zh' ? '结果历史' : 'Result history');
    assert.equal(locale.histogram.title, lang === 'zh' ? '直方图' : 'Histogram');
  }
  if (process.argv.includes('--frontend-only')) { console.log('PASS frontend formats, oversized batching and history/histogram translations'); return; }
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'synco-import-check-'));
  const discovery = path.join(temp, 'interop.json');
  const child = spawn(process.env.SYNCO_BIN || path.join(root, 'target/debug/synco.exe'), [], {
    cwd: root, windowsHide: true, stdio: ['ignore', 'ignore', 'pipe'],
    env: { ...process.env, SYNCO_DATA: path.join(temp, 'data'), SYNCO_PORT: '0', SYNCO_PUBLIC: path.join(root, 'public'), SYNCO_INTEROP_DISCOVERY: discovery },
  });
  let log = '', spawnError; child.stderr.on('data', b => log += b); child.on('error', e => spawnError = e);
  const exited = new Promise(resolve => child.once('exit', resolve));
  try {
    const info = await until(async () => {
      if (spawnError) throw spawnError;
      if (child.exitCode !== null) throw Error(log);
      try { const found = JSON.parse(fs.readFileSync(discovery)); const res = await fetch(found.base_url + '/api/interop/v1/info'); return res.ok && await res.json(); } catch { return null; }
    });
    for (const field of ['max_pixels', 'max_edge', 'max_file_bytes']) assert.equal(info[field], null);
    async function post(route, body) {
      const response = await fetch(info.base_url + route, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
      return { status: response.status, body: await response.json() };
    }
    const bmp = Buffer.alloc(58); bmp.write('BM'); bmp.writeUInt32LE(58, 2); bmp.writeUInt32LE(54, 10);
    bmp.writeUInt32LE(40, 14); bmp.writeInt32LE(1, 18); bmp.writeInt32LE(1, 22); bmp.writeUInt16LE(1, 26); bmp.writeUInt16LE(24, 28);
    // 合法 BMP 的尾随填充使请求超过旧 80 MiB；只解码 1×1，不耗费大幅像素缓冲。
    const payload = { name: 'large.bmp', b64: Buffer.concat([bmp, Buffer.alloc(61 * 1024 ** 2)]).toString('base64'), w: 0, h: 0 };
    const created = await post('/api/projects', { name: 'Import probe', files: [payload] });
    assert.equal(created.status, 200, JSON.stringify(created.body));
    const appended = await post(`/api/projects/${created.body.id}/images`, { files: [payload] });
    assert.equal(appended.status, 200, JSON.stringify(appended.body));
    payload.b64 = '';
    const raw = await post(`/api/projects/${created.body.id}/images`, { files: [{ name: 'source.dng', b64: Buffer.from('II\x2a\x00\x08\x00\x00\x00', 'binary').toString('base64'), w: 1, h: 1 }] });
    assert.equal(raw.status, 400); assert.equal(raw.body.code, 'srv.image.unsupportedFormat');
    const source = path.join(temp, 'large.bmp'); fs.writeFileSync(source, bmp); fs.truncateSync(source, 256 * 1024 ** 2 + 1);
    const hash = crypto.createHash('sha256'); for await (const chunk of fs.createReadStream(source)) hash.update(chunk);
    const spec = { task_id: crypto.randomUUID(), client_id: 'import-check', source_project_id: 'probe', source_image_id: 'large', project_name: 'Large interop', input: { path: source, sha256: hash.digest('hex') }, output: { format: 'png' }, open_in_editor: false };
    const accepted = await post('/api/interop/v1/tasks', spec); assert.equal(accepted.status, 202, JSON.stringify(accepted.body));
    await until(async () => {
      const response = await fetch(`${info.base_url}/api/interop/v1/tasks/${spec.task_id}?client_id=${spec.client_id}`);
      const task = await response.json(); if (task.state === 'failed') throw Error(JSON.stringify(task)); return task.state === 'editing';
    });
    assert.equal(fs.statSync(source).size, 256 * 1024 ** 2 + 1);
    console.log('PASS >80 MiB project create/add, >256 MiB external handoff, unlimited discovery, RAW rejection, frontend formats, oversized single-file batching and independent history/histogram translations');
  } finally {
    child.kill(); await exited;
    assert(temp.startsWith(path.join(os.tmpdir(), 'synco-import-check-')));
    fs.rmSync(temp, { recursive: true, force: true, maxRetries: 6, retryDelay: 100 });
  }
}
main().catch(e => { console.error(e); process.exitCode = 1; });
