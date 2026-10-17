// Isolated API and optional real-browser acceptance for photo development.
// Build the server first. --browser uses PLAYWRIGHT_MODULE or installed playwright.
const fs = require('fs');
const path = require('path');
const os = require('os');
const zlib = require('zlib');
const assert = require('assert/strict');
const { spawn } = require('child_process');
const ROOT = path.resolve(__dirname, '..');
const sleep = ms => new Promise(r => setTimeout(r, ms));
function png(w, h) {
  const data = Buffer.alloc((w * 3 + 1) * h);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) { const i = y * (w * 3 + 1) + 1 + x * 3; data[i] = Math.min(255,40+x/w*180+(x%7)*3);data[i+1]=Math.min(255,35+y/h*180+(y%5)*3);data[i+2]=Math.min(255,180-x/w*80); }
  const crc = b => {let n=0xffffffff;for(const v of b){n^=v;for(let k=0;k<8;k++)n=(n>>>1)^((n&1)?0xedb88320:0);}return (n^0xffffffff)>>>0;};
  const chunk = (name,b) => {const t=Buffer.from(name),head=Buffer.alloc(4),tail=Buffer.alloc(4);head.writeUInt32BE(b.length);tail.writeUInt32BE(crc(Buffer.concat([t,b])));return Buffer.concat([head,t,b,tail]);};
  const head=Buffer.alloc(13);head.writeUInt32BE(w);head.writeUInt32BE(h,4);head[8]=8;head[9]=2;
  return Buffer.concat([Buffer.from([137,80,78,71,13,10,26,10]),chunk('IHDR',head),chunk('IDAT',zlib.deflateSync(data)),chunk('IEND',Buffer.alloc(0))]);
}
async function main() {
  const data=fs.mkdtempSync(path.join(os.tmpdir(),'synco-develop-'));
  const port=21000+Math.floor(Math.random()*18000), base=`http://127.0.0.1:${port}`;
  const bin=process.env.SYNCO_BIN||path.join(ROOT,'target/debug/synco.exe');
  const child=spawn(bin,[],{cwd:ROOT,env:{...process.env,SYNCO_DATA:data,SYNCO_PORT:String(port),SYNCO_PUBLIC:path.join(ROOT,'public')},stdio:['ignore','ignore','pipe'],windowsHide:true});
  let stderr='';child.stderr.on('data',b=>stderr+=b);let browser;
  const exited=new Promise(resolve=>child.once('exit',resolve));
  const request=async(p,body)=>{const r=await fetch(base+p,body===undefined?{}:{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)});const v=await r.json();assert(r.ok,`${r.status} ${JSON.stringify(v)}`);return v;};
  try {
    let ready=false;for(let i=0;i<100;i++){try{if((await fetch(base+'/api/version')).ok){ready=true;break;}}catch{}await sleep(200);}assert(ready,stderr);
    const project=await request('/api/projects',{name:'Development acceptance',files:[{name:'chart.png',b64:png(900,600).toString('base64'),w:900,h:600}]});
    const id=project.image_ids[0], prefix=`/api/images/${id}/adjust`;
    let info;for(let i=0;i<80;i++){info=await request(`/api/images/${id}`);if(info.thumb_url)break;await sleep(150);}assert(info.orig_url);
    const automatic=await request(prefix+'/auto',{});assert.equal(typeof automatic.color.exposure,'number');
    const balance=await request(prefix+'/auto',{point:[0.5,0.5]});assert.equal(typeof balance.color.temp,'number');
    const cube='LUT_1D_SIZE 2\n0 0 0\n1 0.9 1\n';
    const imported=await request(prefix+'/lut',{name:'portrait.cube',text:cube});
    assert(imported.name.endsWith('.cube'));assert.equal((await request(prefix+'/lut',{name:'portrait.cube',text:cube})).name,imported.name);
    const changed=await request(prefix+'/lut',{name:'portrait.cube',text:cube.replace('0.9','0.8')});assert.notEqual(changed.name,imported.name);
    const invalid=await fetch(base+prefix+'/lut',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({name:'bad.cube',text:'not a cube'})});assert.equal(invalid.status,400);
    const ops={color:{exposure:12,temp:5},develop:{look:{basic:{whites:12,blacks:-8},hsl:Array.from({length:8},(_,i)=>({hue:i===1?12:0,saturation:i===5?20:0,luminance:0})),curves:{red:[[0,0],[0.5,0.58],[1,1]]},grading:{shadows:{hue:220,saturation:12}}},detail:{luma:20,chroma:25},effects:{grain:15,vignette:-15,seed:42},locals:[{enabled:true,mask:{kind:'radial',center:[0.5,0.5],radius:[0.25,0.2]},color:{exposure:20}}]}};
    const saved=await request(prefix,{ops});assert(saved.ops.develop.locals.length===1);assert.equal(saved.ops.develop.effects.seed,42);
    const restored=await request(prefix);assert.deepEqual(restored.ops,saved.ops);
    const p=await request(prefix+'/preview',{});assert.equal(p.identity,false);assert.equal(p.w,900);assert((await fetch(base+p.preview_url)).ok);
    assert.equal(p.histogram.red.length,256);assert.equal(p.histogram.red.reduce((a,b)=>a+b,0),900*600);assert.equal(p.histogram.clipping.length,p.histogram.map_width*p.histogram.map_height);assert.deepEqual((await request(prefix+'/preview',{})).histogram,p.histogram);
    const render=await request(prefix+'/render',{});assert.equal(render.w,900);assert.equal(render.h,600);assert((await fetch(base+render.url)).ok);
    const repeat=await request(prefix+'/render',{});assert.equal(repeat.url,render.url);assert(repeat.reused);
    const unsupported=await request(prefix+'/gpu',{ops:saved.ops});assert.equal(unsupported.supported,false);
    const gpu=await request(prefix+'/gpu',{ops:{color:{exposure:12},develop:{look:ops.develop.look}}});assert.equal(gpu.supported,true);assert.equal(gpu.rgba.length,33**3*4);
    const dormant=await request(prefix,{ops:{develop:{locals:[{mask:{kind:'brush',strokes:[{points:[[0.3,0.4],[0.5,0.6]],radius:0.04}]}}]}}});assert.equal(dormant.ops.develop.locals[0].mask.strokes.length,1);
    assert.equal((await request(prefix+'/preview',{})).identity,true);
    console.log('PASS automatic tone, WB, complete parameter roundtrip, proxy, full render, deterministic reuse, GPU support gate, dormant mask persistence');
    await request(prefix,{ops:{}});
    if(process.argv.includes('--browser')){
      const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
      browser=await chromium.launch({channel:process.env.TEST_BROWSER_CHANNEL||'chrome',headless:true,args:['--enable-unsafe-swiftshader']});
      const page=await browser.newPage({viewport:{width:1440,height:1080},deviceScaleFactor:1});
      const errors=[];page.on('pageerror',e=>{errors.push(e.message);console.log('BROWSER ERROR',e.message);});page.on('console',m=>{if(m.type()==='error'){errors.push(m.text());console.log('CONSOLE',m.text());}});
      await page.addInitScript(()=>{localStorage.setItem('synco.theme','light');localStorage.setItem('synco.appearance','glass');});
      await page.goto(`${base}/#/p/${project.id}/e/${id}`);
      console.log('browser route',page.url());
      await page.locator('.prop-tabs [data-k="adjust"]').click();
      const panel=page.locator('.develop-panel');await panel.waitFor();
      const histogram=page.locator('.histogram');await histogram.waitFor();await page.waitForFunction(()=>document.querySelector('.histogram__status')?.textContent.includes('像素'));
      const hudBox=await histogram.boundingBox(), stageBox=await page.locator('.ed-stage').boundingBox();assert(Math.abs(stageBox.x+stageBox.width-hudBox.x-hudBox.width-14)<2);assert(Math.abs(hudBox.y-stageBox.y-14)<2);
      await histogram.locator('[data-channel="luminance"]').click();assert.equal(await histogram.locator('[data-channel="luminance"]').getAttribute('aria-pressed'),'true');await histogram.locator('[data-channel="rgb"]').click();
      await histogram.getByRole('button',{name:'显示高光溢出区域',exact:true}).click();await page.locator('.histogram__clipping').waitFor({state:'visible'});
      await histogram.getByRole('button',{name:'折叠直方图',exact:true}).click();assert(await histogram.locator('.histogram__body').isHidden());await histogram.getByRole('button',{name:'展开直方图',exact:true}).click();
      const sections=await panel.locator(':scope > .grp').evaluateAll(nodes=>nodes.map(n=>n.dataset.section));
      assert.deepEqual(sections,['basic','color','curves','hsl','grading','detail','lens','effects','local','beauty']);
      assert.equal(await panel.locator('[data-section="basic"] .input--num').count(),7);
      await page.route('**/adjust/preview',async route=>{await sleep(500);await route.continue();});
      const exposure=panel.locator('[data-section="basic"] .input--num').first();await exposure.fill('0.15');await exposure.press('Enter');
      await page.locator('.develop-gpu').waitFor({state:'visible'});await page.locator('.develop-gpu').waitFor({state:'hidden'});await page.unroute('**/adjust/preview');
      await page.waitForFunction(async id=>(await(await fetch(`/api/images/${id}/adjust`)).json()).ops.color.exposure===10,id);
      const expand=async key=>{const group=panel.locator(`[data-section="${key}"]`);if(await group.evaluate(n=>n.classList.contains('is-collapsed')))await group.locator(':scope > .develop-heading > .grp__hd').click();return group;};
      const hsl=await expand('hsl');await hsl.locator('.develop-swatch').nth(1).click();await hsl.locator('.input--num').nth(1).fill('25');await hsl.locator('.input--num').nth(1).press('Enter');
      const curves=await expand('curves');const canvas=curves.locator('canvas');await canvas.click({position:{x:135,y:75}});
      await page.waitForFunction(async id=>(await(await fetch(`/api/images/${id}/adjust`)).json()).ops.develop?.look.curves.rgb.length===3,id);
      const locals=await expand('local');await locals.getByRole('button',{name:'添加局部调整',exact:true}).click();
      await locals.getByRole('button',{name:'编辑蒙版',exact:true}).click();
      const overlay=page.locator('.develop-mask-overlay');await overlay.waitFor({state:'visible'});const box=await overlay.boundingBox();
      await page.mouse.move(box.x+box.width*0.5,box.y+box.height*0.5);await page.mouse.down();await page.mouse.move(box.x+box.width*0.7,box.y+box.height*0.7,{steps:5});await page.mouse.up();
      await page.locator('.local-mask-confirm').click();
      await overlay.waitFor({state:'hidden'});
      await locals.locator('input[aria-label="曝光"]').fill('0.3');await locals.locator('input[aria-label="曝光"]').press('Enter');
      await page.waitForFunction(async id=>(await(await fetch(`/api/images/${id}/adjust`)).json()).ops.develop?.locals[0]?.color.exposure===20,id);
      await page.waitForFunction(()=>document.querySelector('.histogram__status')?.textContent.includes('像素'));
      const beforeMove=(await request(prefix)).ops.develop.locals[0].mask;
      const imageBox=await page.locator('.vp__canvas').boundingBox();
      const hover=page.locator('.develop-mask-hover');
      await page.mouse.move(imageBox.x+imageBox.width*.5,imageBox.y+imageBox.height*.5);await hover.waitFor({state:'visible'});
      assert(await hover.evaluate(c=>c.getContext('2d').getImageData(Math.floor(c.width*.5),Math.floor(c.height*.5),1,1).data[3]>0));
      const hoverOutput=path.join(ROOT,'tmp/develop-smoke');fs.mkdirSync(hoverOutput,{recursive:true});await page.locator('.ed-stage').screenshot({path:path.join(hoverOutput,'mask-hover.png')});
      assert.deepEqual((await request(prefix)).ops.develop.locals[0].mask,beforeMove);
      await page.keyboard.down('Space');assert(await hover.isHidden());await page.keyboard.up('Space');
      await page.mouse.move(imageBox.x+imageBox.width*.51,imageBox.y+imageBox.height*.5);await hover.waitFor({state:'visible'});
      await page.mouse.down();await page.mouse.move(imageBox.x+imageBox.width*.61,imageBox.y+imageBox.height*.55,{steps:5});
      await hover.waitFor({state:'visible'});await page.mouse.up();
      for(let i=0;i<50;i++){if((await request(prefix)).ops.develop.locals[0].mask.center[0]>.58)break;await sleep(100);}
      const moved=(await request(prefix)).ops.develop.locals[0];assert(moved.mask.center[0]>.58,JSON.stringify(moved.mask));assert.deepEqual(moved.mask.radius,beforeMove.radius);assert.equal(moved.color.exposure,20);assert(Math.abs(moved.mask.center[1]-(beforeMove.center[1]+.05))<.015,JSON.stringify({beforeMove,moved,imageBox}));assert(await overlay.isHidden());
      await hover.waitFor({state:'visible'});
      await page.mouse.move(imageBox.x+imageBox.width*.02,imageBox.y+imageBox.height*.02);await hover.waitFor({state:'hidden'});
      await locals.getByRole('button',{name:'编辑蒙版',exact:true}).click();await overlay.waitFor({state:'visible'});
      assert.equal((await request(prefix)).ops.develop.locals[0].mask.center[0],moved.mask.center[0]);
      await page.locator('.local-mask-confirm').click();await overlay.waitFor({state:'hidden'});
      // A cancelled move restores the whole mask; it must not become an AI redraw stroke.
      await page.waitForFunction(()=>document.querySelector('.histogram__status')?.textContent.includes('像素'));
      await page.mouse.move(imageBox.x+imageBox.width*.6,imageBox.y+imageBox.height*.55);await page.mouse.down();await page.mouse.move(imageBox.x+imageBox.width*.65,imageBox.y+imageBox.height*.6,{steps:3});
      await page.locator('.vp__canvas').dispatchEvent('pointercancel',{pointerId:1});await page.mouse.up();
      await sleep(250);assert.deepEqual((await request(prefix)).ops.develop.locals[0].mask,moved.mask);
      await locals.getByRole('button',{name:'添加局部调整',exact:true}).click();await locals.getByLabel('蒙版类型',{exact:true}).selectOption('brush');
      await locals.getByRole('button',{name:'编辑蒙版',exact:true}).click();await overlay.waitFor({state:'visible'});
      const brushBox=await overlay.boundingBox();await page.mouse.move(brushBox.x+brushBox.width*.3,brushBox.y+brushBox.height*.3);await page.mouse.down();await page.mouse.move(brushBox.x+brushBox.width*.45,brushBox.y+brushBox.height*.35,{steps:6});await page.mouse.up();
      await page.locator('.local-mask-confirm').click();await overlay.waitFor({state:'hidden'});await page.waitForFunction(()=>document.querySelector('.histogram__status')?.textContent.includes('像素'));
      let brush;for(let i=0;i<50;i++){brush=(await request(prefix)).ops.develop.locals[1]?.mask;if(brush?.strokes.length)break;await sleep(100);}assert(brush.strokes[0].points.length>1);
      await page.mouse.move(brushBox.x+brushBox.width*.3,brushBox.y+brushBox.height*.3);await hover.waitFor({state:'visible'});await page.mouse.down();await page.mouse.move(brushBox.x+brushBox.width*.4,brushBox.y+brushBox.height*.4,{steps:5});await page.mouse.up();
      let brushMoved;for(let i=0;i<50;i++){brushMoved=(await request(prefix)).ops.develop.locals[1].mask;if(brushMoved.strokes[0].points[0][0]>.38)break;await sleep(100);}
      assert(brushMoved.strokes[0].points[0][0]>.38);assert.equal(brushMoved.strokes[0].points.length,brush.strokes[0].points.length);assert.equal(brushMoved.strokes[0].radius,brush.strokes[0].radius);
      assert.deepEqual((await request(prefix)).ops.develop.locals[0].mask,moved.mask);
      // Moving on a rotated/flipped preview still writes source coordinates.
      await page.evaluate(async()=>{await (await import('/public/js/views/editor/index.js')).beforeLeaveEditor();});
      const geometric=(await request(prefix)).ops;geometric.geometry.rotate_deg=90;geometric.geometry.flip_h=true;await request(prefix,{ops:geometric});
      await page.reload();await page.locator('.prop-tabs [data-k="adjust"]').click();await expand('local');
      await page.waitForFunction(()=>document.querySelector('.develop-panel')?.getAttribute('aria-busy')==='false' && document.querySelector('.histogram__status')?.textContent.includes('像素'));
      const rotatedBox=await page.locator('.vp__canvas').boundingBox(),startX=1-moved.mask.center[1],startY=1-moved.mask.center[0];
      await page.mouse.move(rotatedBox.x+rotatedBox.width*startX,rotatedBox.y+rotatedBox.height*startY);await hover.waitFor({state:'visible'});
      assert(await hover.evaluate((c,p)=>c.getContext('2d').getImageData(Math.floor(c.width*p[0]),Math.floor(c.height*p[1]),1,1).data[3]>0,[startX,startY]));
      await page.mouse.down();await page.mouse.move(rotatedBox.x+rotatedBox.width*(startX+.05),rotatedBox.y+rotatedBox.height*(startY+.1),{steps:5});await page.mouse.up();
      let rotated;for(let i=0;i<50;i++){rotated=(await request(prefix)).ops;if(rotated.develop.locals[0].mask.center[0]<moved.mask.center[0]-.08)break;await sleep(100);}
      assert(Math.abs(rotated.develop.locals[0].mask.center[0]-(moved.mask.center[0]-.1))<.015,JSON.stringify({rotated:rotated.develop.locals[0].mask,moved:moved.mask,rotatedBox,geo:rotated.geometry}));assert(Math.abs(rotated.develop.locals[0].mask.center[1]-(moved.mask.center[1]-.05))<.015);
      await page.evaluate(async()=>{await (await import('/public/js/views/editor/index.js')).beforeLeaveEditor();});
      rotated.geometry.rotate_deg=0;rotated.geometry.flip_h=false;await request(prefix,{ops:rotated});await page.reload();await page.locator('.prop-tabs [data-k="adjust"]').click();await expand('local');
      const effects=await expand('effects');await effects.locator('input[type="file"]').setInputFiles({name:'portrait.cube',mimeType:'text/plain',buffer:Buffer.from(cube)});
      await page.waitForFunction(async id=>!!(await(await fetch(`/api/images/${id}/adjust`)).json()).ops.lut?.name,id);
      const effectSelect=effects.locator('select');await effectSelect.scrollIntoViewIfNeeded();await effectSelect.focus();
      const screenDir=path.join(ROOT,'tmp/develop-smoke');fs.mkdirSync(screenDir,{recursive:true});await page.locator('.ed-prop').screenshot({path:path.join(screenDir,'effects-fixed.png')});
      const localToolbar=locals.locator('.develop-local-tools');await localToolbar.scrollIntoViewIfNeeded();await localToolbar.locator('select').focus();await page.locator('.ed-prop').screenshot({path:path.join(screenDir,'local-fixed.png')});
      const geometryCheck=await panel.locator('select:visible').evaluateAll(nodes=>nodes.map(n=>{const s=getComputedStyle(n);return {label:n.getAttribute('aria-label'),inner:n.clientHeight-parseFloat(s.paddingTop)-parseFloat(s.paddingBottom),font:parseFloat(s.fontSize)};}));assert(geometryCheck.length>=3);assert(geometryCheck.every(v=>v.inner>=v.font*1.2),JSON.stringify(geometryCheck));
      await page.reload();await page.locator('.prop-tabs [data-k="adjust"]').click();await panel.waitFor();
      const persisted=await request(prefix);assert.equal(persisted.ops.develop.look.hsl[1].saturation,25);assert.equal(persisted.ops.develop.locals[0].color.exposure,20);
      await page.waitForFunction(()=>document.querySelector('.develop-panel [data-section="basic"] input[aria-label="曝光"]')?.value==='0.15');
      await panel.locator('[data-section="basic"] .develop-reset').click();await page.waitForFunction(async id=>(await(await fetch(`/api/images/${id}/adjust`)).json()).ops.color.exposure===0,id);
      const second=await request(`/api/projects/${project.id}/images`,{files:[{name:'second.png',b64:png(120,80).toString('base64'),w:120,h:80}]});
      const beforeTemp=(await request(prefix)).ops.color.temp;
      let releaseAuto;const autoGate=new Promise(r=>releaseAuto=r);await page.route('**/adjust/auto',async route=>{await autoGate;await route.continue();});
      await panel.getByRole('button',{name:'白平衡吸管',exact:true}).click();try{await overlay.waitFor({state:'visible',timeout:5000});}catch(e){console.log(await page.evaluate(()=>({poster:{src:document.querySelector('.vp__poster').src,complete:document.querySelector('.vp__poster').complete,width:document.querySelector('.vp__poster').naturalWidth},stage:document.querySelector('.ed-stage').className,text:document.querySelector('.ed-prop').innerText,canvas:document.querySelector('.develop-mask-overlay').outerHTML})));throw e;}
      const autoSent=page.waitForRequest(r=>r.url().endsWith('/adjust/auto'));await overlay.click({position:{x:60,y:60}});await autoSent;
      const switchTo=async image=>{const loaded=page.waitForResponse(r=>r.url().endsWith(`/api/images/${image}/adjust`)&&r.request().method()==='GET');await page.evaluate(hash=>location.hash=hash,`#/p/${project.id}/e/${image}`);await loaded;};
      await switchTo(second.ids[0]);await switchTo(id);
      const autoReturned=page.waitForResponse(r=>r.url().endsWith('/adjust/auto'));releaseAuto();await autoReturned;await page.evaluate(()=>new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r))));
      assert.equal((await request(prefix)).ops.color.temp,beforeTemp);await page.unroute('**/adjust/auto');
      await page.waitForFunction(()=>document.querySelector('.develop-panel')?.getAttribute('aria-busy')==='false' && document.querySelector('.histogram__status')?.textContent.includes('像素'));
      const output=path.join(ROOT,'tmp/develop-smoke');fs.mkdirSync(output,{recursive:true});await page.screenshot({path:path.join(output,'editor.png')});await page.locator('.ed-prop').screenshot({path:path.join(output,'panel.png')});
      const beauty=await expand('beauty');await beauty.locator('.grp__hd').first().scrollIntoViewIfNeeded();assert(await beauty.getByRole('button',{name:'开始画形',exact:true,includeHidden:true}).count());
      const overflow=await panel.evaluate(n=>({width:n.clientWidth,scroll:n.scrollWidth,right:Math.round(n.getBoundingClientRect().right),columnRight:Math.round(n.closest('.ed-prop').getBoundingClientRect().right),nodes:[...n.querySelectorAll('*')].filter(e=>e.getBoundingClientRect().width>0&&e.getBoundingClientRect().right>n.closest('.ed-prop').getBoundingClientRect().right+1).map(e=>[e.className,Math.round(e.getBoundingClientRect().width),e.textContent.slice(0,45)]).slice(0,12)}));assert(overflow.scroll<=overflow.width+1&&overflow.right<=overflow.columnRight+1,JSON.stringify(overflow));
      await page.locator('.ed-prop').screenshot({path:path.join(output,'beauty.png')});
      assert.deepEqual(errors,[]);console.log('PASS browser histogram placement/channels/clipping, dropdown layout, explicit mask confirmation, direct radial/brush movement, cancel rollback, independent layers, section order, EV, HSL, curves, LUT, reload, GPU/CPU, WB A-B-A, beauty nesting, no page errors');
    }
  } finally {
    if(browser)await browser.close();child.kill();await exited;
    if(process.argv.includes('--keep'))console.log('DATA',data);else fs.rmSync(data,{recursive:true,force:true,maxRetries:6,retryDelay:150});
  }
}
main().catch(e=>{console.error(e);process.exitCode=1;});
