'use strict';
// 审计修复回归：退出成功表示故障触发条件下保持正确行为。
const fs = require('node:fs'), path = require('node:path'), os = require('node:os');
const vm = require('node:vm'), http = require('node:http'), assert = require('node:assert/strict');
const { DatabaseSync } = require('node:sqlite');
const root = path.resolve(__dirname, '..');
const read = rel => fs.readFileSync(path.join(root, rel), 'utf8');
const deferred = () => { let resolve; const promise = new Promise(r => resolve = r); return { promise, resolve }; };
const turn = () => new Promise(r => setImmediate(r));

async function frontend() {
  const source = read('public/js/views/editor/index.js');
  const funcs = source.slice(source.indexOf('async function doSubmitCloud(snapshot)'), source.indexOf('/** 任务终态'));
  function env(flush) {
    const requests = [];
    const values = { cloud: { key_saved: true }, settings: { full: true, prompt: 'before' }, images: [{ id: 1, has_mask: true }, { id: 2, has_mask: true }] };
    const ctx = { imgId: 1, adjust: { flush }, painter: { flush: async () => true }, params: { stages: { reset() {}, set() {} }, setBusy() {}, line() {} }, compare: { hide() {} } };
    let generation = 0, occupied = false;
    const sandbox = { structuredClone, captureAction: () => { const id = ctx.imgId, epoch = generation; return { imgId: id, projectId: 1, current: () => ctx.imgId === id && generation === epoch }; }, claimSubmission: () => { if (occupied) return null; occupied = true; return { release: () => { occupied = false; } }; }, ctx, store: { peek: k => values[k] }, isCloud: () => true, saveSettings: async () => {}, cloudPrompt: s => s.prompt, api: { cloudEdit: async body => { requests.push(body); return { result_id: requests.length }; } }, setJob() {}, adopt() {}, toastErr() {}, t: s => s, dx: s => s, lastSubmitted: null, rerunFrom: null, onJobSettled() {} };
    vm.createContext(sandbox); vm.runInContext(funcs, sandbox);
    return { sandbox, requests, values, nextSession: () => ++generation };
  }
  const waiting = deferred(), a = env(() => waiting.promise);
  const one = a.sandbox.doSubmit(), two = a.sandbox.doSubmit();
  waiting.resolve(); await Promise.all([one, two]);
  assert.equal(a.requests.length, 1);
  console.log(JSON.stringify({ probe: 'editor-double-submit', acceptedCalls: a.requests.length }));
  const switching = deferred(), b = env(() => switching.promise);
  const inFlight = b.sandbox.doSubmit(); b.sandbox.ctx.imgId = 2; switching.resolve(); await inFlight;
  assert.equal(b.requests.length, 0);
  console.log(JSON.stringify({ probe: 'submit-switch-during-save', clickedImage: 1, generationCalls: b.requests.length }));

  const abaSave = deferred(), aba = env(() => abaSave.promise);
  const abaRun = aba.sandbox.doSubmit(); aba.sandbox.ctx.imgId = 2; aba.nextSession(); aba.sandbox.ctx.imgId = 1; aba.nextSession();
  abaSave.resolve(); await abaRun; assert.equal(aba.requests.length, 0);
  const freezeSave = deferred(), frozen = env(() => freezeSave.promise);
  const frozenRun = frozen.sandbox.doSubmit(); frozen.values.settings.prompt = 'after'; frozen.values.cloud.model = 'new-model';
  freezeSave.resolve(); await frozenRun;
  assert.equal(frozen.requests[0].settings.prompt, 'before');
  assert.notEqual(frozen.requests[0].settings.cloud.model, 'new-model');
  let saved = false;
  const retry = env(async () => saved);
  await retry.sandbox.doSubmit(); assert.equal(retry.requests.length, 0);
  saved = true; await retry.sandbox.doSubmit(); assert.equal(retry.requests.length, 1);
  console.log('submission snapshot, A→B→A cancellation, and failure release passed');

  const adjust = read('public/js/views/editor/adjust/index.js');
  const commit = adjust.slice(adjust.indexOf('  function commit()'), adjust.indexOf('  function applyPreview('));
  const c = { structuredClone, saving: Promise.resolve(true), dirty: true, rev: 1, seq: 0, idOf: () => 1, ops: {}, mode: 'off', api: { saveAdjust: async () => { throw Error('injected save failure'); } }, line() {}, t: s => s, short: e => e.message, applyPreview() {} };
  vm.createContext(c); vm.runInContext(commit, c); assert.equal(await c.commit(), false);
  assert.equal(c.dirty, true);
  console.log(JSON.stringify({ probe: 'adjust-save-failure', result: false, stillDirty: c.dirty }));
  const failedSave = env(() => c.commit());
  failedSave.sandbox.ctx.painter.flush = async () => false;
  await failedSave.sandbox.doSubmit();
  assert.equal(failedSave.requests.length, 0);
  console.log(JSON.stringify({ probe: 'submit-after-failed-save', adjustmentDirty: c.dirty, painterSaved: false, generationCalls: failedSave.requests.length }));

  const painterSource = read('public/js/views/editor/paint.js').replace(/^import .*;\r?\n/gm, '').replace(/export /g, '');
  const drawing = { clearRect() {}, drawImage() {}, getImageData: () => ({ data: new Uint8Array(4) }) };
  const canvas = () => ({ width: 4, height: 4, getContext: () => drawing, addEventListener() {} });
  let acceptPaint = false;
  const mask = canvas(), paintEnv = { Uint8Array, t: s => s, document: { createElement: canvas }, window: { addEventListener() {}, removeEventListener() {} }, setTimeout: () => 1, clearTimeout() {}, cancelAnimationFrame() {} };
  vm.createContext(paintEnv); vm.runInContext(painterSource, paintEnv);
  const painter = paintEnv.createPainter({ mask, onSaved: async () => acceptPaint });
  await painter.load(4,4,null,1); painter.clear();
  await assert.rejects(painter.flush(), /ed.maskSaveFail/);
  await assert.rejects(painter.load(8,8,null,2), /ed.maskSaveFail/);
  assert.equal(mask.width, 4); assert.equal(mask.height, 4);
  acceptPaint = true; assert.equal(await painter.flush(), true); await painter.load(8,8,null,2);
  assert.equal(mask.width,8); painter.detach();
  console.log('painter failure blocks resize, retains draft, and permits retry passed');

  let resultSource = read('public/js/views/editor/workspace/results.js').replace(/^import .*;\r?\n/gm, '').replace(/export /g, '');
  const pending = deferred(); let current = { imgId: 1, info: null, history: { setResults() {} } };
  const d = { api: { delResult: async () => {}, image: async () => pending.promise }, confirm: async () => true, t: s => s, toastOk() {}, toastErr() {}, syncExport() {} };
  vm.createContext(d); vm.runInContext(resultSource, d);
  const actions = d.createResultActions({ capture: () => { const owner = current; return { imgId: owner.imgId, projectId: 1, current: () => current === owner }; }, context: () => current, syncExport() {} });
  const deleting = actions.delResult({ id: 11 }); await turn();
  current = { imgId: 2, info: null, history: { setResults() {} } };
  pending.resolve({ id: 1, results: [], last: null }); await deleting;
  assert.equal(current.info, null);
  console.log(JSON.stringify({ probe: 'delete-result-stale-response', currentImage: current.imgId, installedInfo: current.info }));
}

async function backend() {
  const helperSource = read('tools/comfy-e2e.js').split('async function main()')[0];
  const helpers = new Function('require', '__dirname', helperSource + '\nreturn {boot, req, userGraph, B64};')(require, path.join(root, 'tools'));
  const data = fs.mkdtempSync(path.join(os.tmpdir(), 'synco-audit-20261012-'));
  const hits = [], mocks = []; let proc, db, complete = false;
  const upload = deferred(); let uploads = 0, submitted = 0;
  async function mock(name) {
    const server = http.createServer((req, res) => {
      req.resume(); req.on('end', () => {
        hits.push({ backend: name, method: req.method, url: req.url });
        let out = {};
        if (req.url === '/upload/image') { out = { name: 'audit.png' }; uploads++; if (uploads === 1) return upload.promise.then(() => { res.writeHead(200, { 'content-type': 'application/json' }); res.end(JSON.stringify(out)); }); }
        if (req.url === '/prompt') out = { prompt_id: `audit-${++submitted}`, number: submitted };
        if (req.url === '/queue') out = { queue_running: name === 'A' ? [[0, 'audit-1']] : [], queue_pending: [] };
        if (req.url.startsWith('/history/') && complete) {
          const pid = req.url.slice('/history/'.length);
          out = { [pid]: { status: { completed: true, status_str: 'success' }, outputs: Object.fromEntries(['51','19','20'].map(id => [id,{images:[{filename:`${id}.png`,subfolder:'',type:'output'}]}])) } };
        }
        if (req.url.startsWith('/view?')) { res.writeHead(200, {'content-type':'image/png'}); res.end(Buffer.from(helpers.B64,'base64')); return; }
        res.writeHead(200, { 'content-type': 'application/json' }); res.end(JSON.stringify(out));
      });
    });
    await new Promise(r => server.listen(0, '127.0.0.1', r)); mocks.push(server);
    return `http://127.0.0.1:${server.address().port}`;
  }
  try {
    const A = await mock('A'), B = await mock('B');
    const boot = await helpers.boot(data); proc = boot.proc;
    const req = (method, url, body) => helpers.req(boot.base, method, url, body);
    for (const url of [A, B]) await req('POST', '/api/backends', { url });
    await req('POST', '/api/backends/select', { url: A });
    const p = await req('POST', '/api/projects', { name: 'isolated-audit', files: [{ name: 'a.png', b64: helpers.B64, w: 2, h: 2 }] });
    const iid = p.body.image_ids[0]; await req('POST', `/api/images/${iid}/mask`, { b64: helpers.B64 });
    const wf = path.join(data, 'audit-wf.json'); fs.writeFileSync(wf, JSON.stringify(helpers.userGraph()));
    await req('POST', '/api/settings/workflow', { path: wf });
    const running = req('POST', '/api/run', { image_ids: [iid], settings: { prompt: 'audit', steps: 22, cfg: 4, seed: 1, loras: [] } });
    const uploadStart = Date.now();
    while (uploads === 0) { assert.ok(Date.now() - uploadStart < 10000); await turn(); }
    await req('POST', '/api/backends/select', { url: B });
    upload.resolve();
    const run = await running;
    assert.equal(hits.filter(h => h.url === '/upload/image' && h.backend === 'A').length, 2);
    assert.ok(hits.some(h => h.url === '/prompt' && h.backend === 'A'));
    assert.ok(!hits.some(h => h.url === '/prompt' && h.backend === 'B'));

    const rid = run.body.results[0].result_id; assert.ok(rid, JSON.stringify(run));
    const selected = await req('POST', '/api/backends/select', { url: B });
    assert.equal(selected.status, 200);
    proc.kill(); if (proc.exitCode === null) await new Promise(r => proc.once('exit', r));
    const restarted = await helpers.boot(data); proc = restarted.proc; boot.base = restarted.base;
    let result;
    const started = Date.now();
    do {
      result = await req('GET', `/api/results/${rid}`);
      if (hits.some(h => h.backend === 'A' && h.url === '/history/audit-1')) break;
      await new Promise(r => setTimeout(r, 100));
    } while (Date.now() - started < 10000);
    assert.equal(result.body.status, 'running');
    assert.ok(hits.some(h => h.backend === 'A' && h.url === '/history/audit-1'));
    assert.ok(!hits.some(h => h.backend === 'B' && h.url === '/history/audit-1'));
    assert.equal((await req('POST', `/api/results/${rid}/interrupt`, {})).status, 200);
    assert.ok(hits.some(h => h.backend === 'A' && h.url === '/interrupt'));
    assert.ok(!hits.some(h => h.backend === 'B' && h.url === '/interrupt'));
    console.log(JSON.stringify({ probe: 'comfy-switch-backend', submittedBackend: 'A', polledBackend: 'A', status: result.body.status, error: result.body.error }));

    complete = true;
    const second = await req('POST', '/api/run', { comfy_url: A, image_ids: [iid], settings: { prompt: 'finish', steps: 22, cfg: 4, seed: 1, loras: [] } });
    const secondId = second.body.results[0].result_id;
    assert.ok(secondId, JSON.stringify(second));
    const deadline = Date.now() + 15000;
    let done;
    do { done = await req('GET', `/api/results/${secondId}`); if (done.body.status === 'done') break; await new Promise(r => setTimeout(r, 100)); } while (Date.now() < deadline);
    assert.equal(done.body.status, 'done');
    assert.ok(hits.some(h => h.backend === 'A' && h.url.startsWith('/view?')));
    assert.ok(!hits.some(h => h.backend === 'B' && h.url.startsWith('/view?')));
    console.log('upload-time switch, restart identity, bound interrupt, completion and download passed');

    await req('POST', `/api/images/${iid}/adjust`, { ops: { color: { exposure: 10 } } });
    db = new DatabaseSync(path.join(data, 'app.db'));
    const ref = `projects/${p.body.id}/audit-ref.png`;
    fs.writeFileSync(path.join(data,ref), Buffer.from(helpers.B64,'base64'));
    db.prepare('INSERT INTO app_settings(key,value) VALUES(?,?)').run(`canvas_refs:${iid}`, JSON.stringify([ref]));
    db.exec("CREATE TRIGGER audit_delete_failure BEFORE DELETE ON projects BEGIN SELECT RAISE(ABORT,'audit-delete-failure'); END;");
    const del = await req('DELETE', `/api/projects/${p.body.project_id || p.body.id}`);
    const state = { projects: db.prepare('SELECT count(*) AS n FROM projects').get().n, images: db.prepare('SELECT count(*) AS n FROM images').get().n, results: db.prepare('SELECT count(*) AS n FROM results').get().n, adjusts: db.prepare('SELECT count(*) AS n FROM image_adjust').get().n };
    assert.equal(del.status, 500); assert.equal(state.projects, 1); assert.equal(state.images, 1); assert.equal(state.results, 2); assert.equal(state.adjusts, 1);
    assert.equal(db.prepare('SELECT value FROM app_settings WHERE key=?').get(`canvas_refs:${iid}`).value, JSON.stringify([ref]));
    assert.ok(fs.existsSync(path.join(data,ref)));
    console.log(JSON.stringify({ probe: 'project-delete-rollback', http: del.status, ...state }));
    db.exec("DROP TRIGGER audit_delete_failure; CREATE TRIGGER audit_image_failure BEFORE DELETE ON images BEGIN SELECT RAISE(ABORT,'audit-image-failure'); END;");
    const delImage = await req('DELETE', `/api/images/${iid}`);
    assert.equal(delImage.status, 500);
    assert.equal(db.prepare('SELECT count(*) AS n FROM results').get().n, 2);
    assert.equal(db.prepare('SELECT count(*) AS n FROM image_adjust').get().n, 1);
    db.exec('DROP TRIGGER audit_image_failure');
    assert.equal((await req('DELETE', `/api/projects/${p.body.id}`)).status, 200);
    for (const table of ['projects','images','results','job_specs','image_adjust','image_face','image_landmark']) assert.equal(db.prepare(`SELECT count(*) AS n FROM ${table}`).get().n, 0, table);
    assert.equal(db.prepare("SELECT count(*) AS n FROM app_settings WHERE key LIKE 'canvas_refs:%' OR key LIKE 'delete_cleanup:%'").get().n, 0);
    console.log('image/project deletion rollback and successful cleanup passed');
    const blocked = 'runtime/audit-cleanup-blocker';
    fs.mkdirSync(path.join(data,blocked));
    db.prepare('INSERT INTO app_settings(key,value) VALUES(?,?)').run('delete_cleanup:retry-probe', JSON.stringify({files:[blocked],dirs:[]}));
    const retryStart = Date.now();
    while (Date.now() - retryStart < 5500) await new Promise(r => setTimeout(r,100));
    assert.ok(db.prepare("SELECT value FROM app_settings WHERE key='delete_cleanup:retry-probe'").get());
    fs.rmdirSync(path.join(data,blocked)); fs.writeFileSync(path.join(data,blocked),'delete me');
    db.close(); db = null;
    proc.kill(); if (proc.exitCode === null) await new Promise(r => proc.once('exit',r));
    const cleanupBoot = await helpers.boot(data); proc=cleanupBoot.proc;
    db = new DatabaseSync(path.join(data,'app.db'));
    const cleanupDeadline = Date.now()+10000;
    while (db.prepare("SELECT value FROM app_settings WHERE key='delete_cleanup:retry-probe'").get() && Date.now()<cleanupDeadline) await new Promise(r=>setTimeout(r,100));
    assert.ok(!db.prepare("SELECT value FROM app_settings WHERE key='delete_cleanup:retry-probe'").get());
    assert.ok(!fs.existsSync(path.join(data,blocked)));
    console.log('failed file cleanup persists and retries after restart passed');


  } finally {
    db?.close(); proc?.kill();
    if (proc && proc.exitCode === null) await new Promise(r => proc.once('exit', r));
    await Promise.all(mocks.map(s => new Promise(r => s.close(r))));
    fs.rmSync(data, { recursive: true, force: true });
  }
}
(async () => { await frontend(); await backend(); })().catch(e => { console.error(e); process.exitCode = 1; });
