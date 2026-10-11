'use strict';
// 架构契约回归：临时资料目录、本地 mock、虚构 key；不读用户 data/。
const fs = require('fs'), path = require('path'), os = require('os'), http = require('http');
const { spawn } = require('child_process');
const { DatabaseSync } = require('node:sqlite');
const root = path.resolve(__dirname, '..');
const pngSource = fs.readFileSync(path.join(root, 'tools/cloud-e2e.js'), 'utf8').split('// ---- 假云端')[0];
const { png } = new Function('require', '__dirname', pngSource + '\nreturn {png};')(require, path.join(root, 'tools'));
const pause = ms => new Promise(r => setTimeout(r, ms));
async function until(fn, ms = 15000) {
  const t = Date.now();
  while (Date.now() - t < ms) { const v = await fn(); if (v) return v; await pause(40); }
  throw new Error('probe timed out');
}
const tempRoot = path.resolve(os.tmpdir());
const data = fs.mkdtempSync(path.join(tempRoot, 'synco-architecture-audit-'));
let proc, db, gateway, release;
const hits = [];
(async () => {
  gateway = http.createServer((req, res) => {
    const bufs = []; req.on('data', b => bufs.push(b)); req.on('end', () => {
      const body = Buffer.concat(bufs), raw = body.toString('latin1');
      const field = name => new RegExp('name="' + name + '"\\r\\n\\r\\n([^\\r\\n]*)').exec(raw)?.[1] || '';
      const at = body.indexOf(Buffer.from([137,80,78,71]));
      if (at < 0) { res.writeHead(400); res.end('{}'); return; }
      const end = body.indexOf('\r\n--', at);
      const image = body.subarray(at, end < 0 ? body.length : end);
      hits.push({ prompt: field('prompt'), model: field('model'), quality: field('quality'), url: req.url, key: req.headers.authorization });
      const send = () => { res.writeHead(200, {'content-type':'application/json'}); res.end(JSON.stringify({data:[{b64_json:image.toString('base64')}]})); };
      if (field('prompt') === 'audit-hold') release = send; else send();
    });
  });
  await new Promise(r => gateway.listen(0, '127.0.0.1', r));
  proc = spawn(path.join(root, 'target/debug/synco.exe'), [], {cwd:root, windowsHide:true, env:{...process.env,SYNCO_DATA:data,SYNCO_PORT:'0'},stdio:['ignore','pipe','pipe']});
  let spawnError;
  proc.on('error', e => { spawnError = e; });
  let log = ''; proc.stdout.on('data', b => log += b); proc.stderr.on('data', b => log += b);
  let base = await until(() => {
    if (spawnError) throw spawnError;
    if (proc.exitCode !== null) throw new Error('isolated server exited: ' + log);
    return /SYNCO_URL=(http:\/\/127\.0\.0\.1:\d+)/.exec(log)?.[1];
  });
  async function api(method, url, body) {
    const r = await fetch(base + url, {method, headers:{'content-type':'application/json'}, ...(body === undefined ? {} : {body:JSON.stringify(body)}), signal:AbortSignal.timeout(15000)});
    const j = await r.json(); if (!r.ok) throw new Error(url + ': ' + JSON.stringify(j)); return j;
  }
  const photo = png(1024,1024,()=>[100,120,140,255]);
  const maskOld = png(1024,1024,(x,y)=>[255,255,255,(x-180)**2+(y-180)**2<60**2?255:0]);
  const maskNew = png(1024,1024,(x,y)=>[255,255,255,(x-800)**2+(y-800)**2<60**2?255:0]);
  const p = await api('POST','/api/projects',{name:'architecture-audit',files:[{name:'first.png',b64:photo.toString('base64'),w:1024,h:1024},{name:'second.png',b64:photo.toString('base64'),w:1024,h:1024}]});
  const [first, second] = p.image_ids;
  await api('POST',`/api/images/${second}/mask`,{b64:maskOld.toString('base64')});
  await api('POST','/api/cloud',{base:`http://127.0.0.1:${gateway.address().port}/v1`,model:'audit-model-a',key:'audit-fake-key',concurrency:1});
  await api('POST','/api/cloud/queue',{image_ids:[first],settings:{prompt:'audit-hold',invert:true,edge:1024}});
  await until(() => release);
  const queued = await api('POST','/api/cloud/queue',{image_ids:[second],settings:{prompt:'audit-queued',edge:1024}});
  const queuedId = queued.results[0].result_id;
  const before = await api('GET',`/api/results/${queuedId}`);
  if (before.status !== 'queued') throw new Error('second job was not queued');
  const full = await api('POST','/api/cloud/queue',{image_ids:[second],settings:{prompt:'audit-full',full:true}});
  const canvas = await api('POST','/api/canvas/create',{name:'snapshot-canvas',w:1024,h:1024});
  const sketch = await api('POST',`/api/canvas/${canvas.image_id}/generate`,{settings:{prompt:'audit-sketch'}});
  await api('POST','/api/cloud',{model:'audit-model-b',base:`http://127.0.0.1:${gateway.address().port}/v2`,quality:'high',key:'audit-new-key',stitch_expand:150,stitch_feather:70,stitch_edge:2048});
  await api('POST',`/api/images/${second}/adjust`,{ops:{geometry:{rotate_deg:90}}});
  await api('POST',`/api/images/${second}/mask`,{b64:maskNew.toString('base64')});
  release();
  const row = await until(async () => { const r=await api('GET',`/api/results/${queuedId}`); if(r.status==='error') throw new Error(JSON.stringify(r)); return r.status==='done' && r.mask_snap_path ? r : false; });
  const usedMask = fs.readFileSync(path.join(data,row.mask_snap_path));
  const drift = {probe:'queued-job-input-drift',status:row.status,recordedModel:row.model,actualModel:hits.find(h=>h.prompt==='audit-queued').model,usedMaskEqualsOriginal:usedMask.equals(maskOld),usedMaskEqualsEdited:usedMask.equals(maskNew)};
  console.log(JSON.stringify(drift));
  if (drift.recordedModel !== drift.actualModel || !drift.usedMaskEqualsOriginal || drift.usedMaskEqualsEdited) throw new Error('queued input drifted');
  const request = hits.find(h=>h.prompt==='audit-queued');
  if (request.url!=='/v1/images/edits' || request.quality!=='medium' || request.key!=='Bearer audit-fake-key') throw new Error('cloud config drifted');
  const snap=JSON.parse(row.snap_json);
  if (snap.ops.geometry.rotate_deg!==0 || snap.stitch.expand!==96 || snap.stitch.crop_edge!==1024) throw new Error('adjust/stitch inputs drifted');
  if (JSON.stringify(row).includes('audit-fake-key') || JSON.stringify(await api('GET','/api/cloud/queue')).includes('audit-fake-key')) throw new Error('credential leaked');
  for (const [id,prompt] of [[full.results[0].result_id,'audit-full'],[sketch.result_id,'audit-sketch']]) {
    await until(async()=>{const r=await api('GET',`/api/results/${id}`);if(r.status==='error')throw new Error(JSON.stringify(r));return r.status==='done';});
    const h=hits.find(h=>h.prompt===prompt);
    if(h.model!=='audit-model-a'||h.url!=='/v1/images/edits'||h.quality!=='medium'||h.key!=='Bearer audit-fake-key')throw new Error(prompt+' config drifted');
  }
  console.log('crop/full/sketch: frozen endpoint, credentials, model and quality passed');
  db = new DatabaseSync(path.join(data,'app.db')); db.exec('PRAGMA busy_timeout=5000');
  if (db.prepare('SELECT count(*) n FROM job_specs WHERE result_id=?').get(queuedId).n) throw new Error('completed task retained its execution credential');
  const legacyId = Number(db.prepare("INSERT INTO results(image_id,project_id,status,backend,prompt,settings_json) VALUES(?,?,'queued','cloud','audit-legacy','{}')").run(second,p.id).lastInsertRowid);
  db.exec("CREATE TRIGGER audit_fail_snapshot BEFORE UPDATE OF raw_path ON results WHEN NEW.raw_path IS NOT NULL BEGIN SELECT RAISE(ABORT, 'audit injected snapshot write failure'); END;");
  const fail = await api('POST','/api/cloud/queue',{image_ids:[second],settings:{prompt:'audit-failure',edge:1024}});
  const failId = fail.results[0].result_id;
  const legacy=await until(async()=>{const r=await api('GET',`/api/results/${legacyId}`);return r.status==='error'?r:false;});
  if(legacy.error!=='srv.queue.specMissing'||hits.some(h=>h.prompt==='audit-legacy'))throw new Error('legacy queued task ran with mutable inputs');
  console.log('legacy queued task: explicit missing-spec error, no cloud request');
  await until(async()=>['done','error'].includes((await api('GET',`/api/results/${failId}`)).status));
  await pause(200);
  const broken = await api('GET',`/api/results/${failId}`);
  const out = await fetch(base+`/api/results/${failId}/lossless`); await out.arrayBuffer();
  await api('DELETE',`/api/results/${failId}`);
  const remaining = fs.readdirSync(path.join(data,'projects',String(p.id))).filter(n=>n.startsWith(`r${failId}_`));
  console.log(JSON.stringify({probe:'result-aggregate-commit',injectedFailure:'UPDATE raw_path aborted in isolated database',statusAfterFailure:broken.status,rawPath:broken.raw_path,maskSnapshot:broken.mask_snap_path,exportDisposition:out.headers.get('content-disposition'),unownedFilesAfterDeletingResult:remaining}));
  if (broken.status !== 'error' || broken.raw_path || broken.final_path || broken.thumb_path || remaining.length || out.ok) throw new Error('partial result was published or files leaked');
  if (db.prepare('SELECT count(*) n FROM job_specs WHERE result_id=?').get(failId).n) throw new Error('deleted result retained its private spec');
  // 模拟落盘后、SQLite 完成提交前进程退出；重启只清理未登记的生成文件。
  db.close(); db=null;
  const stopped=new Promise(r=>proc.once('exit',r));proc.kill();await stopped;
  const orphan=path.join(data,'projects',String(p.id),`r${failId}_1791680000000_raw.png`);
  fs.writeFileSync(orphan,photo);
  proc=spawn(path.join(root,'target/debug/synco.exe'),[],{cwd:root,windowsHide:true,env:{...process.env,SYNCO_DATA:data,SYNCO_PORT:'0'},stdio:['ignore','pipe','pipe']});
  spawnError=null;proc.on('error',e=>spawnError=e);log='';
  proc.stdout.on('data',b=>log+=b);proc.stderr.on('data',b=>log+=b);
  base=await until(()=>{if(spawnError)throw spawnError;if(proc.exitCode!==null)throw new Error(log);return /SYNCO_URL=(http:\/\/127\.0\.0\.1:\d+)/.exec(log)?.[1];});
  if(fs.existsSync(orphan)||!fs.existsSync(path.join(data,row.raw_path))||!fs.existsSync(path.join(data,row.final_path)))throw new Error('restart cleanup removed registered files or retained an orphan');
  console.log('restart: unregistered output removed, completed materials preserved');
})().catch(e=>{console.error(e.stack);process.exitCode=1;}).finally(async()=>{
  if(db) db.close();
  if(proc?.pid && proc.exitCode===null){const done=new Promise(r=>proc.once('exit',r));proc.kill();await done;}
  if(gateway){gateway.closeAllConnections();await new Promise(r=>gateway.close(r));}
  const resolved=path.resolve(data);
  if(path.dirname(resolved)!==tempRoot || !path.basename(resolved).startsWith('synco-architecture-audit-')) throw new Error('unsafe audit cleanup target');
  fs.rmSync(resolved,{recursive:true,force:true});
});
