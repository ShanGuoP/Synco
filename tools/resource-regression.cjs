// 单实例 24MP 混跑：本地 mock，不读用户资料；输出进程内存与共同准入指标。
'use strict';
const fs=require('node:fs'),path=require('node:path'),os=require('node:os'),http=require('node:http'),assert=require('node:assert/strict');
const {spawn}=require('node:child_process');
const root=path.resolve(__dirname,'..');
const source=fs.readFileSync(path.join(root,'tools/cloud-e2e.js'),'utf8').split('async function main()')[0];
const helpers=new Function('require','__dirname',source+'\nreturn {png,boot,req};')(require,__dirname);
const pause=ms=>new Promise(r=>setTimeout(r,ms));
async function until(fn,ms=90000){const start=Date.now();while(Date.now()-start<ms){const v=await fn();if(v)return v;await pause(150);}throw Error('resource probe timed out');}
const data=fs.mkdtempSync(path.join(os.tmpdir(),'synco-resources-'));
let proc,mock,sampler;
const held=[],samples=[],errors=[];
let phase='startup';
(async()=>{
 mock=http.createServer((req,res)=>{
  const chunks=[];req.on('data',b=>chunks.push(b));req.on('end',()=>{
   const body=Buffer.concat(chunks), text=body.toString('latin1');
   const at=body.indexOf(Buffer.from([137,80,78,71]));const end=body.indexOf('\r\n--',at);
   const png=body.subarray(at,end<0?body.length:end);
   const send=()=>{if(res.destroyed)return;res.writeHead(200,{'content-type':'application/json'});res.end(JSON.stringify({data:[{b64_json:png.toString('base64')}]}));};
   if(/HOLD-RESOURCE/.test(text))held.push(send);else send();
  });
 });await new Promise(r=>mock.listen(0,'127.0.0.1',r));
 const boot=await helpers.boot(data);proc=boot.proc;
 const req=(method,url,body)=>helpers.req(boot.base,method,url,body,120000);
 assert.equal((await req('POST','/api/cloud',{kind:'cloud',base:`http://127.0.0.1:${mock.address().port}/v1`,model:'identity',key:'sk-mock-only',concurrency:'4'})).status,200);
 if(process.platform==='win32'){
  const command=`$p=Get-Process -Id ${proc.pid}; while(-not $p.HasExited){$p.Refresh(); [pscustomobject]@{rss=$p.WorkingSet64; peakRss=$p.PeakWorkingSet64; private=$p.PrivateMemorySize64} | ConvertTo-Json -Compress; Start-Sleep -Milliseconds 200}`;
  sampler=spawn('pwsh',['-NoProfile','-Command',command],{windowsHide:true,stdio:['ignore','pipe','pipe']});
  let pending='';sampler.stdout.on('data',b=>{pending+=b;let at;while((at=pending.indexOf('\n'))>=0){const line=pending.slice(0,at).trim();pending=pending.slice(at+1);try{samples.push({...JSON.parse(line),phase});}catch{}}});
  sampler.stderr.on('data',b=>errors.push(String(b)));
 }
 phase='import';
 const photo=helpers.png(4000,6000,(x,y)=>[40+x%180,55+y%160,110,255]);
 const project=await req('POST','/api/projects',{name:'resource-probe',files:[{name:'24MP.png',w:1,h:1,b64:photo.toString('base64')}]});
 assert.equal(project.status,200,JSON.stringify(project));const iid=project.body.image_ids[0];
 const imported=await req('GET',`/api/images/${iid}`);assert.equal(imported.body.w,4000);assert.equal(imported.body.h,6000);
 const mask=helpers.png(640,960,(x,y)=>[255,255,255,(x-320)**2+(y-480)**2<60**2?255:0]);
 await req('POST',`/api/images/${iid}/mask`,{b64:mask.toString('base64')});
 await req('POST',`/api/images/${iid}/adjust`,{ops:{color:{exposure:5}}});
 const baseline=await req('POST','/api/cloud/queue',{image_ids:[iid],settings:{prompt:'identity'}});
 const baseId=baseline.body.results[0].result_id;
 await until(async()=>{const r=await req('GET',`/api/results/${baseId}`);assert.notEqual(r.body.status,'error',JSON.stringify(r));return r.body.status==='done';});
 phase='network-wait';
 const queue=await req('POST','/api/cloud/queue',{image_ids:[iid,iid,iid,iid],settings:{prompt:'HOLD-RESOURCE'}});
 const ids=queue.body.results.map(r=>r.result_id);assert.equal(ids.length,4);
 await until(()=>held.length===4);
 await until(async()=>{const r=await req('GET','/api/cloud/queue');return r.body.image_work.active===0&&r.body.running===4;});
 const jobs=()=>fs.existsSync(path.join(data,'runtime/jobs'))?fs.readdirSync(path.join(data,'runtime/jobs')).filter(n=>n.startsWith('crop-')):[];
 assert.equal(jobs().length,4);
 phase='all-cloud-held';
 // 给 OS 采样器足够时间记录四次网络等待，期间没有全幅内存保留。
 await pause(800);
 phase='mixed';
 const workload=[
  ...Array.from({length:3},()=>req('POST',`/api/images/${iid}/adjust/preview`,{})),
  ...Array.from({length:2},()=>req('POST',`/api/images/${iid}/adjust/render`,{})),
  req('POST','/api/projects',{name:'concurrent-import',files:[{name:'24MP-2.png',w:4000,h:6000,b64:photo.toString('base64')}]}),
  req('POST',`/api/results/${baseId}/fork`,{}),
  fetch(boot.base+`/api/results/${baseId}/lossless`,{signal:AbortSignal.timeout(120000)}).then(async r=>{assert.equal(r.status,200);await r.arrayBuffer();return {status:r.status};}),
 ];
 const responses=await Promise.all(workload);for(const r of responses)assert.equal(r.status,200,JSON.stringify(r));
 phase='cancellation';
 for(const id of ids.slice(0,2))assert.equal((await req('POST',`/api/results/${id}/interrupt`,{})).status,200);
 await until(()=>jobs().length===2);
 await until(async()=>{const r=await req('GET','/api/cloud/queue');return r.body.image_work.active===0;});
 for(const send of held)send();
 phase='completion';
 await until(async()=>{for(const id of ids.slice(2)){const r=await req('GET',`/api/results/${id}`);assert.notEqual(r.body.status,'error',JSON.stringify(r));if(r.body.status!=='done')return false;}return true;});
 await until(()=>jobs().length===0);
 const final=await until(async()=>{const r=await req('GET','/api/cloud/queue');return r.body.image_work.active===0&&r.body.image_work.reserved_mib===0?r.body:null;});
 assert.ok(final.image_work.peak_active<=2);assert.ok(final.image_work.peak_active>=1);
 assert.ok(final.image_work.peak_reserved_mib<=final.image_work.budget_mib);
 await pause(300);
 const memory=Object.fromEntries([...new Set(samples.map(s=>s.phase))].map(p=>[p,{sampleCount:samples.filter(s=>s.phase===p).length,peakRss:Math.max(...samples.filter(s=>s.phase===p).map(s=>s.rss)),peakPrivate:Math.max(...samples.filter(s=>s.phase===p).map(s=>s.private))}]));
 const evidence={image:{w:4000,h:6000},logicalCores:os.cpus().length,binary:process.env.SYNCO_BIN||'target/debug/synco.exe',binarySha256:require('node:crypto').createHash('sha256').update(fs.readFileSync(process.env.SYNCO_BIN||path.join(root,'target/debug/synco.exe'))).digest('hex'),singleInstance:true,cloudHeld:4,mixed:['import','preview x3','render x2','fork','lossless export','cloud x4'],imageWork:final.image_work,memory,processLifetimePeakRss:Math.max(0,...samples.map(s=>s.peakRss)),samplerErrors:errors,temporaryJobDirectories:jobs().length};
 assert.ok(process.platform!=='win32'||samples.length>0,'missing Windows memory samples');
 fs.writeFileSync(path.join(root,'docs/audit-2026-10-12/resources-evidence.json'),JSON.stringify(evidence,null,2));
 console.log(JSON.stringify(evidence));
})().catch(e=>{console.error(e);process.exitCode=1;}).finally(async()=>{
 for(const send of held)send();
 if(proc){proc.kill();if(proc.exitCode===null)await new Promise(r=>proc.once('exit',r));}
 if(mock)await new Promise(r=>mock.close(r));
 if(sampler&&sampler.exitCode===null)await Promise.race([new Promise(r=>sampler.once('exit',r)),pause(3000)]);
 fs.rmSync(data,{recursive:true,force:true});
});
