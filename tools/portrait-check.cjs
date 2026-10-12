'use strict';
// 合成两张脸的隔离 HTTP 回归。不访问真实照片、模型接口或用户资料库。
const {spawn}=require('node:child_process');
const {DatabaseSync}=require('node:sqlite');
const fs=require('node:fs'),os=require('node:os'),path=require('node:path'),assert=require('node:assert/strict');
const root=path.resolve(__dirname,'..');
const data=fs.mkdtempSync(path.join(os.tmpdir(),'synco-portrait-'));
const port=20000+Math.floor(Math.random()*20000),base=`http://127.0.0.1:${port}`;
const child=spawn(process.env.SYNCO_BIN||path.join(root,'target/debug/synco.exe'),[],{env:{...process.env,SYNCO_DATA:data,SYNCO_PORT:String(port)},windowsHide:true,stdio:['ignore','ignore','pipe']});
let errors='';child.stderr.on('data',b=>errors+=b);
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
async function req(method,url,body,status=200){
 const r=await fetch(base+url,{method,headers:body?{'content-type':'application/json'}:{},body:body?JSON.stringify(body):undefined,signal:AbortSignal.timeout(60000)});
 const j=await r.json();assert.equal(r.status,status,JSON.stringify(j));return j;
}
function bmp(){
 const w=640,h=480,stride=w*3,b=Buffer.alloc(54+stride*h);
 b.write('BM');b.writeUInt32LE(b.length,2);b.writeUInt32LE(54,10);b.writeUInt32LE(40,14);b.writeInt32LE(w,18);b.writeInt32LE(h,22);b.writeUInt16LE(1,26);b.writeUInt16LE(24,28);
 for(let y=0;y<h;y++)for(let x=0;x<w;x++){const i=54+(h-1-y)*stride+x*3;b[i]=110+x%50;b[i+1]=120+y%70;b[i+2]=155+x%70;}
 return b;
}
function mesh(cx){
 const points=Array.from({length:468},()=>[cx,0.48]);
 const ring=(ids,c,r)=>ids.forEach((id,j)=>{const a=-Math.PI/2+j/ids.length*Math.PI*2;points[id]=[c[0]+r[0]*Math.cos(a),c[1]+r[1]*Math.sin(a)];});
 ring([10,338,297,332,284,251,389,356,454,323,361,288,397,365,379,378,400,377,152,148,176,149,150,136,172,58,132,93,234,127,162,21,54,103,67,109],[cx,0.48],[0.18,0.32]);
 ring([33,7,163,144,145,153,154,155,133,173,157,158,159,160,161,246],[cx-0.075,0.39],[0.045,0.025]);
 ring([263,249,390,373,374,380,381,382,362,398,384,385,386,387,388,466],[cx+0.075,0.39],[0.045,0.025]);
 ring([61,146,91,181,84,17,314,405,321,375,291,409,270,269,267,0,37,39,40,185],[cx,0.64],[0.075,0.038]);
 ring([78,95,88,178,87,14,317,402,318,324,308,415,310,311,312,13,82,81,80,191],[cx,0.64],[0.052,0.018]);
 ring([70,63,105,66,107,55,65,52,53,46],[cx-0.075,0.32],[0.052,0.016]);ring([300,293,334,296,336,285,295,282,283,276],[cx+0.075,0.32],[0.052,0.016]);
 points[98]=[cx-0.035,0.51];points[327]=[cx+0.035,0.51];points[1]=[cx,0.51];return points;
}
let cleaned=false;
async function cleanup(){
 if(cleaned)return;cleaned=true;child.kill();await sleep(600);
 const resolved=fs.realpathSync(data);assert.equal(path.dirname(resolved).toLowerCase(),fs.realpathSync(os.tmpdir()).toLowerCase());assert(path.basename(resolved).startsWith('synco-portrait-'));
 fs.rmSync(resolved,{recursive:true,force:true,maxRetries:6,retryDelay:200});
}
(async()=>{
 try{
  for(let i=0;i<60;i++){try{if((await fetch(base+'/api/version')).ok)break;}catch{}await sleep(200);}
  const project=await req('POST','/api/projects',{name:'Portrait feature test',files:[{name:'two-faces.bmp',b64:bmp().toString('base64'),w:640,h:480}]});
  const id=project.image_ids[0],endpoint=`/api/images/${id}/adjust`;
  const db=new DatabaseSync(path.join(data,'app.db'));db.exec('PRAGMA busy_timeout=5000');
  const saveMesh=db.prepare('INSERT INTO image_landmark(image_id,idx,points) VALUES(?,?,?)');
  const saveFace=db.prepare('INSERT INTO image_face(image_id,idx,box,landmarks,score) VALUES(?,?,?,?,?)');
  for(let idx=0;idx<2;idx++){const cx=idx?0.75:0.25;saveMesh.run(id,idx,JSON.stringify(mesh(cx)));saveFace.run(id,idx,JSON.stringify([cx-0.18,0.16,0.36,0.64]),JSON.stringify([[cx,0.35],[cx-0.075,0.39],[cx+0.075,0.39],[cx,0.51],[cx,0.64]]),0.99);}
  db.close();
  const panel=await req('GET',endpoint);assert.equal(panel.face_analysis.faces.length,2);assert(panel.face_analysis.faces.every(f=>f.has_landmarks));
  const portrait={faces:[{idx:1,auto:{eye_big:20},shape:{cheekbone:30,eye_distance:-20,nose_tip:30,mouth_width:20},beauty:{smooth:15,texture:50},retouch:{dark_circles:40,eye_bags:20,teeth:50,eye_light:40,rosy:20,redness:10,yellow:20},makeup:{blush:20,lipstick:40,brows:20,gloss:25},lip_color:[180,65,85],blush_color:[220,110,125],protect_features:true,protect_background:true}],body:{region:[0.15,0.1,0.7,0.85],strength:{waist:30,shoulders:20,arms:20,legs:20,long_legs:20}}};
  const saved=await req('POST',endpoint,{ops:{portrait}});assert.equal(saved.ops.portrait.faces[0].idx,1);assert.equal(saved.ops.portrait.faces[0].makeup.lipstick,40);
  const a=await req('POST',endpoint+'/preview',{}),b=await req('POST',endpoint+'/preview',{});assert.equal(a.preview_url,b.preview_url);
  const full=await req('POST',endpoint+'/render',{});assert(full.url);
  const restored=await req('GET',endpoint);assert.deepEqual(restored.ops.portrait,saved.ops.portrait);
  const other={...portrait,faces:[{idx:0,retouch:{rosy:40}},{idx:1,makeup:{lipstick:60}}]};
  const multi=await req('POST',endpoint,{ops:{portrait:other,geometry:{rotate_deg:90,flip_h:true,crop:[0.1,0.1,0.8,0.8]}}});assert.equal(multi.ops.portrait.faces.length,2);
  const rotated=await req('POST',endpoint+'/preview',{});assert(rotated.preview_url!==a.preview_url);
  await req('POST',endpoint,{ops:{portrait:{faces:[{idx:1,beauty:{smooth:20,by_mask:true}}]}}});await req('POST',endpoint+'/render',{});
  await req('POST',endpoint,{ops:{portrait:{faces:'invalid'}}},400);
  await req('POST',endpoint,{ops:{}});
  console.log('Portrait HTTP checks passed: two faces, independent parameters, all groups, persistent restore, immutable preview, full render, geometry, empty painted mask, invalid input.');
  if(process.argv.includes('--serve')){
   console.log(JSON.stringify({url:base+`/#/p/${project.id}/e/${id}`,data,id,port}));
   await new Promise(resolve=>{process.once('SIGINT',resolve);process.once('SIGTERM',resolve);});
  }
 }catch(e){console.error(e,errors);process.exitCode=1;}
 finally{await cleanup();}
})();
