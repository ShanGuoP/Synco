// 双向交接的隔离验收；所有资料、发现文件和故障注入都限制在本脚本创建的临时目录。
const fs=require('fs'),path=require('path'),os=require('os'),assert=require('assert/strict'),crypto=require('crypto'),zlib=require('zlib');
const {spawn,spawnSync}=require('child_process');
const ROOT=path.resolve(__dirname,'..'),sleep=ms=>new Promise(r=>setTimeout(r,ms));
const sha=b=>crypto.createHash('sha256').update(b).digest('hex');
function png(w,h){
  const crc=b=>{let n=0xffffffff;for(const v of b){n^=v;for(let k=0;k<8;k++)n=(n>>>1)^((n&1)?0xedb88320:0);}return(n^0xffffffff)>>>0;};
  const chunk=(name,b)=>{const t=Buffer.from(name),a=Buffer.alloc(4),c=Buffer.alloc(4);a.writeUInt32BE(b.length);c.writeUInt32BE(crc(Buffer.concat([t,b])));return Buffer.concat([a,t,b,c]);};
  const head=Buffer.alloc(13);head.writeUInt32BE(w);head.writeUInt32BE(h,4);head[8]=8;head[9]=2;
  const pixels=Buffer.alloc((w*3+1)*h);for(let y=0;y<h;y++)for(let x=0;x<w;x++){const i=y*(w*3+1)+1+x*3;pixels[i]=50+x%120;pixels[i+1]=60+y%100;pixels[i+2]=90;}
  return Buffer.concat([Buffer.from([137,80,78,71,13,10,26,10]),chunk('IHDR',head),chunk('IDAT',zlib.deflateSync(pixels)),chunk('IEND',Buffer.alloc(0))]);
}
async function main(){
  const temp=fs.mkdtempSync(path.join(os.tmpdir(),'synco-interop-'));let data=path.join(temp,'library');fs.mkdirSync(data);
  const source=path.join(temp,'source.png'),bytes=png(320,240);fs.writeFileSync(source,bytes);
  const discovery=path.join(temp,'interop.json');let child,exited,base,browser,stderr='';
  async function start(){
    child=spawn(path.join(ROOT,'target/debug/synco.exe'),[],{cwd:ROOT,env:{...process.env,SYNCO_DATA:data,SYNCO_PORT:'0',SYNCO_PUBLIC:path.join(ROOT,'public'),SYNCO_INTEROP_DISCOVERY:discovery},stdio:['ignore','ignore','pipe'],windowsHide:true});
    child.stderr.on('data',b=>stderr+=b);exited=new Promise(r=>child.once('exit',r));
    for(let i=0;i<100;i++){try{const found=JSON.parse(fs.readFileSync(discovery));const r=await fetch(found.base_url+'/api/interop/v1/info');const info=await r.json();if(r.ok&&info.instance_id===found.instance_id){base=found.base_url;return info;}}catch{}await sleep(100);}throw Error('Server did not start: '+stderr);
  }
  async function stop(){if(child){child.kill();await exited;child=null;}}
  async function request(p,body,status=200){const r=await fetch(base+p,body===undefined?{}:{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)});const v=await r.json();assert.equal(r.status,status,JSON.stringify(v));return v;}
  const prefix='/api/interop/v1/tasks';
  const make=(overrides={})=>({task_id:crypto.randomUUID(),client_id:'photo-library',source_project_id:'album-001',project_name:'Interop acceptance',source_image_id:crypto.randomUUID(),input:{path:source,sha256:sha(bytes)},output:{format:'jpeg',quality:95},open_in_editor:false,...overrides});
  const task=s=>request(`${prefix}/${s.task_id}?client_id=${s.client_id}`);
  async function wait(s,state){for(let i=0;i<160;i++){const t=await task(s);if(t.state===state)return t;if(t.state==='failed'&&state!=='failed')throw Error(JSON.stringify(t));await sleep(100);}throw Error('Timed out: '+JSON.stringify(await task(s)));}
  try{
    const info=await start();assert.equal(info.protocol,1);assert.equal((await request('/api/version')).version,'0.4.2');
    assert.deepEqual(info.input_formats,['jpeg','png','webp','bmp']);
    await request(prefix,make({input:{path:'relative.png',sha256:sha(bytes)}}),400);
    await request(prefix,make({output:{format:'raw',quality:95}}),415);
    const first=make();const accepted=await request(prefix,first,202);assert.equal(accepted.state,'importing');const editing=await wait(first,'editing');
    fs.unlinkSync(source);assert.equal((await request(prefix,first)).image_id,editing.image_id);
    await request(prefix,{...first,project_name:'Changed content'},409);
    fs.writeFileSync(source,bytes);
    const second=make({output:{format:'png',quality:95}});await request(prefix,second,202);const secondEditing=await wait(second,'editing');assert.equal(secondEditing.project_id,editing.project_id);
    const sql=statement=>{const p=spawnSync('python',['-c','import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.executescript(sys.argv[2]); c.commit()',path.join(data,'app.db'),statement],{windowsHide:true});assert.equal(p.status,0,p.stderr?.toString());};
    const returns=()=>{const d=path.join(data,'interop/returns');return fs.existsSync(d)?fs.readdirSync(d).sort():[];};
    const previousFiles=returns();
    sql("CREATE TRIGGER deny_interop_result BEFORE UPDATE OF state ON interop_tasks WHEN NEW.state='ready' BEGIN SELECT RAISE(ABORT,'injected publication failure'); END;");
    await request(`${prefix}/${second.task_id}/complete`,{client_id:second.client_id},202);
    for(let i=0;i<100;i++){const t=await task(second);if(t.state==='editing'&&t.last_error)break;await sleep(100);}
    assert.equal((await task(second)).state,'editing');assert((await task(second)).last_error);assert.deepEqual(returns(),previousFiles);
    sql('DROP TRIGGER deny_interop_result;');
    const peer=make({client_id:'other-library',task_id:first.task_id});await request(prefix,peer,202);assert.notEqual((await wait(peer,'editing')).project_id,editing.project_id);
    await request(`${prefix}/${first.task_id}?client_id=missing-client`,undefined,404);
    const deleted=await fetch(base+`/api/images/${editing.image_id}`,{method:'DELETE'});assert.equal(deleted.status,409);
    await request(`/api/images/${editing.image_id}/adjust`,{ops:{color:{exposure:20}}});
    const complete=await request(`${prefix}/${first.task_id}/complete`,{client_id:first.client_id},202);assert(['rendering','ready'].includes(complete.state));
    const ready=await wait(first,'ready');assert.equal(sha(fs.readFileSync(ready.output.path)),ready.output.sha256);assert.equal(ready.output.format,'jpeg');assert.equal(ready.output.width,320);
    await request(`${prefix}/${first.task_id}/complete`,{client_id:first.client_id},202);assert.equal((await task(first)).result_id,ready.result_id);
    const pending=()=>request(prefix+'?client_id=photo-library&delivery=pending&limit=50');
    assert((await pending()).tasks.some(t=>t.task_id===first.task_id));assert((await pending()).tasks.some(t=>t.task_id===first.task_id));
    await request(`${prefix}/${first.task_id}/ack`,{client_id:first.client_id,result_id:ready.result_id,sha256:'0'.repeat(64)},409);
    const bad=make({input:{path:source,sha256:'0'.repeat(64)}});await request(prefix,bad,202);assert.equal((await wait(bad,'failed')).last_error.code,'hash_mismatch');
    await request(`${prefix}/${bad.task_id}/ack`,{client_id:bad.client_id});assert(!(await pending()).tasks.some(t=>t.task_id===bad.task_id));
    const cancel=make();await request(prefix,cancel,202);await request(`${prefix}/${cancel.task_id}/cancel`,{client_id:cancel.client_id});assert.equal((await task(cancel)).state,'cancelled');
    console.log('PASS durable import, project association, source preservation, idempotency, tenant isolation, export checksum, publication-failure cleanup, repeated completion, pending delivery, ACK conflict, failure and cancellation');
    if(process.argv.includes('--browser')){
      const{chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');browser=await chromium.launch({channel:'chrome',headless:true});const page=await browser.newPage({viewport:{width:1440,height:1000}});const errors=[];page.on('pageerror',e=>errors.push(e.message));
      await page.goto(base+'/#/');
      const incoming=make({source_project_id:'ui-album',open_in_editor:true});await request(prefix,incoming,202);const target=await wait(incoming,'editing');
      await page.waitForURL(`**/#/p/${target.project_id}/e/${target.image_id}`);await page.getByRole('button',{name:'完成并返回',exact:true}).waitFor();
      const next=make({source_project_id:'ui-album',open_in_editor:true});await request(prefix,next,202);await wait(next,'editing');await sleep(1900);assert(page.url().endsWith('/e/'+target.image_id));
      await page.getByRole('button',{name:'完成并返回',exact:true}).click();await wait(incoming,'ready');
      await page.getByText('已完成，等待整理软件领取',{exact:true}).waitFor();
      const screen=path.join(ROOT,'tmp/interop-smoke');fs.mkdirSync(screen,{recursive:true});await page.screenshot({path:path.join(screen,'return-ready.png')});
      assert.deepEqual(errors,[]);await browser.close();browser=null;
      console.log('PASS browser automatic opening, no interruption by a second push, finish-and-return action and offline collection state');
    }
    await stop();
    // 关闭隔离实例后注入中断状态，验证重启恢复与未登记文件清理。
    const injected=spawnSync('python',['-c',"import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute(\"UPDATE interop_tasks SET state='rendering' WHERE task_id=? AND client_id=?\",(sys.argv[2],sys.argv[3])); c.commit()",path.join(data,'app.db'),second.task_id,second.client_id],{windowsHide:true});assert.equal(injected.status,0,injected.stderr?.toString());
    const orphan=path.join(data,'interop/returns/orphan.part');fs.mkdirSync(path.dirname(orphan),{recursive:true});fs.writeFileSync(orphan,'incomplete');
    const movedData=path.join(temp,'moved-library');fs.cpSync(data,movedData,{recursive:true});data=movedData;
    const restarted=await start();assert.notEqual(restarted.instance_id,info.instance_id);assert.equal((await task(second)).state,'editing');assert(!fs.existsSync(path.join(data,'interop/returns/orphan.part')));
    const after=await task(first);assert.equal(after.state,'ready');assert.equal(after.result_id,ready.result_id);assert(after.output.path.includes('moved-library'));assert.equal(sha(fs.readFileSync(after.output.path)),ready.output.sha256);
    // 用隔离库中的已完成成图模拟 AI 输出，不调用真实模型。
    const secondImage=await request(`/api/images/${secondEditing.image_id}`);const relative=decodeURIComponent(secondImage.orig_url.slice('/file/'.length));
    sql(`INSERT INTO results(image_id,project_id,status,final_path) VALUES(${secondEditing.image_id},${secondEditing.project_id},'done','${relative.replaceAll("'","''")}');`);
    const result=(await request(`/api/images/${secondEditing.image_id}`)).results.find(r=>r.status==='done');assert(result);
    await request(`${prefix}/${peer.task_id}/complete`,{client_id:peer.client_id,result_id:result.id},400);
    await request(`${prefix}/${second.task_id}/complete`,{client_id:second.client_id,result_id:result.id},202);const pngResult=await wait(second,'ready');
    assert.equal(pngResult.source_result_id,result.id);assert.equal(pngResult.output.format,'png');assert.deepEqual(fs.readFileSync(pngResult.output.path).subarray(0,8),Buffer.from([137,80,78,71,13,10,26,10]));
    // 接收端先独立保存，再 ACK；重复 ACK 不重复创建版本。
    const received=path.join(temp,'received.jpg');fs.copyFileSync(after.output.path,received);assert.equal(sha(fs.readFileSync(received)),after.output.sha256);
    const receipt={client_id:first.client_id,result_id:after.result_id,sha256:after.output.sha256};await request(`${prefix}/${first.task_id}/ack`,receipt);await request(`${prefix}/${first.task_id}/ack`,receipt);assert.equal((await task(first)).state,'delivered');assert(!(await pending()).tasks.some(t=>t.task_id===first.task_id));
    const removed=await fetch(base+`/api/images/${editing.image_id}`,{method:'DELETE'});assert.equal(removed.status,200);assert(fs.existsSync(received));assert.equal(sha(fs.readFileSync(received)),after.output.sha256);
    console.log('PASS restart recovery, orphan cleanup, discovery renewal, result path after library relocation, receiver copy, repeat ACK and deletion after delivery');
  }finally{if(browser)await browser.close();await stop();if(!temp.startsWith(path.join(os.tmpdir(),'synco-interop-')))throw Error('Unexpected cleanup path');fs.rmSync(temp,{recursive:true,force:true,maxRetries:6,retryDelay:150});}
}
main().catch(e=>{console.error(e);process.exitCode=1;});
