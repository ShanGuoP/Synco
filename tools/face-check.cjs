'use strict';
// Isolated HTTP test with a user-provided portrait; no project data is touched.
// SYNCO_FACE_TEST_IMAGE=<portrait.jpg> node tools/face-check.cjs
const { spawn } = require('node:child_process');
const fs = require('node:fs'), os = require('node:os'), path = require('node:path');
const assert = require('node:assert/strict');
const ROOT = path.resolve(__dirname, '..');
const BIN = process.env.SYNCO_BIN || path.join(ROOT, 'target/debug/synco.exe');
const portrait = process.env.SYNCO_FACE_TEST_IMAGE;
assert(portrait && fs.existsSync(portrait), 'Set SYNCO_FACE_TEST_IMAGE to a portrait');
const portraitBytes = fs.readFileSync(portrait);
function dimensions(b) {
  if (b.subarray(1,4).toString()==='PNG') return {w:b.readUInt32BE(16),h:b.readUInt32BE(20)};
  for (let i=2;i+9<b.length;i++) {
    if (b[i]!==255) continue;
    const m=b[i+1];
    if (m>=0xc0 && m<=0xcf && m!==0xc4 && m!==0xc8 && m!==0xcc) return {w:b.readUInt16BE(i+7),h:b.readUInt16BE(i+5)};
    const len=b.readUInt16BE(i+2); if (len>1) i+=len+1;
  }
  throw new Error('Use a JPEG or PNG portrait');
}
const data = fs.mkdtempSync(path.join(os.tmpdir(), 'synco-face-e2e-'));
const port = 20000 + Math.floor(Math.random() * 20000);
const base = `http://127.0.0.1:${port}`;
const child = spawn(BIN, [], { env: { ...process.env, SYNCO_DATA: data, SYNCO_PORT: String(port) }, windowsHide: true, stdio: ['ignore', 'ignore', 'pipe'] });
let stderr = ''; child.stderr.on('data', b => { stderr += b; });
const sleep = ms => new Promise(r => setTimeout(r, ms));
async function req(method, endpoint, body) {
  const r = await fetch(base + endpoint, { method, headers: body ? { 'content-type': 'application/json' } : {}, body: body ? JSON.stringify(body) : undefined, signal: AbortSignal.timeout(60000) });
  const json = await r.json(); assert.equal(r.status, 200, JSON.stringify(json)); return json;
}
function blankBmp() {
  const b = Buffer.alloc(54 + 64 * 64 * 3);
  b.write('BM'); b.writeUInt32LE(b.length, 2); b.writeUInt32LE(54, 10);
  b.writeUInt32LE(40, 14); b.writeInt32LE(64, 18); b.writeInt32LE(64, 22);
  b.writeUInt16LE(1, 26); b.writeUInt16LE(24, 28); return b;
}
(async () => {
  try {
    for (let i=0;i<60;i++) {
      try { if ((await fetch(base+'/api/version')).ok) break; } catch {}
      await sleep(300);
    }
    const project = await req('POST','/api/projects',{name:'Face test',files:[
      {name:path.basename(portrait),b64:portraitBytes.toString('base64'),...dimensions(portraitBytes)},
      {name:'blank.bmp',b64:blankBmp().toString('base64'),w:64,h:64},
    ]});
    const [id,blank] = project.image_ids; assert(id && blank);
    const endpoint = `/api/images/${id}/adjust`;
    assert.equal((await req('GET',endpoint)).face_analysis.analyzed,false);
    const analysis = await req('POST',endpoint+'/faces',{});
    assert(analysis.count>0 && analysis.has_landmarks && analysis.analyzed);
    assert.equal(analysis.suggestions.length,6);
    const panel = await req('GET',endpoint);
    assert.equal(panel.has_landmarks,true); assert.equal(panel.face_analysis.analyzed,true);
    const ops = {warp:{auto:{face_slim:20,eye_big:12,nose_slim:10,chin:8}},beauty:{smooth:25,texture:50,auto_face:true}};
    const saved = await req('POST',endpoint,{ops}); assert.equal(saved.ops.beauty.auto_face,true);
    const a = await req('POST',endpoint+'/preview',{});
    const b = await req('POST',endpoint+'/preview',{});
    assert.equal(a.preview_url,b.preview_url); assert(a.preview_url);
    const rendered = await req('POST',endpoint+'/render',{}); assert(rendered.url);
    for (const angle of [0,90,-12]) {
      const crop = await req('POST',endpoint+'/face-crop',{ops:{geometry:{rotate_deg:angle,flip_h:true}},ratio:0.75});
      assert.equal(crop.crop.length,4); assert(crop.crop.every(v=>v>=0 && v<=1));
    }
    const none = await req('POST',`/api/images/${blank}/adjust/faces`,{});
    assert.equal(none.count,0); assert.equal(none.has_landmarks,false); assert.equal(none.analyzed,true);
    const blankPanel=await req('GET',`/api/images/${blank}/adjust`); assert.equal(blankPanel.face_analysis.analyzed,true);
    await req('POST',`/api/images/${blank}/adjust`,{ops:{beauty:{smooth:50,brighten:50,auto_face:true}}});
    await req('POST',`/api/images/${blank}/adjust/render`,{});
    await req('DELETE',`/api/images/${id}`);
    console.log('Face HTTP checks passed: detection, persistent mesh, six crops, shape/skin save, preview reuse, full render, rotated composition, empty detection, deletion.');
  } catch(e) { console.error(e,stderr); process.exitCode=1; }
  finally {
    child.kill(); await sleep(500);
    const resolved = fs.realpathSync(data);
    assert.equal(path.dirname(resolved).toLowerCase(), fs.realpathSync(os.tmpdir()).toLowerCase());
    assert(path.basename(resolved).startsWith('synco-face-e2e-'));
    fs.rmSync(resolved,{recursive:true,force:true,maxRetries:6,retryDelay:150});
  }
})();
