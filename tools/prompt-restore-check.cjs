'use strict';
// Real browser regression: production parameter panel + history restore action.
// No generation requests or user data; serve public modules on an isolated port.
// Set SYNCO_PLAYWRIGHT_MODULE when Playwright is outside the Node search path.
const fs = require('node:fs'), path = require('node:path'), http = require('node:http');
const assert = require('node:assert/strict');
const { chromium } = require(process.env.SYNCO_PLAYWRIGHT_MODULE || 'playwright');
const publicRoot = path.resolve(__dirname, '../public');
const defaults = JSON.parse(fs.readFileSync(path.resolve(__dirname, '../crates/server/defaults.json'), 'utf8'));
const negative = defaults.negative;
const pageHtml = `<!doctype html><html><body><button id="restore">Restore history</button>
<script type="module">
import { store, cloudPrompt } from '/js/state.js';
import { createParams } from '/js/views/editor/params.js';
import { createResultActions } from '/js/views/editor/workspace/results.js';
import { use } from '/js/core/i18n.js';
use('zh', await (await fetch('/locales/zh.json')).json(), {boot:true});
const negative=${JSON.stringify(negative)};
const config={negative,steps:20,cfg:3,loras:[]};
store.set({cfg:config,settings:{prompt:'Edit portrait',negative,steps:24,cfg:4,seed:10,randomSeed:false,loras:[],mode:'cloud'},cloud:{kind:'cloud'}});
window.payloads=[];
const params=createParams({onSubmit:()=>window.payloads.push(cloudPrompt(store.peek('settings')))});
document.body.append(params.node);
params.sync(store.peek('settings'));
window.record={id:77,backend:'cloud',settings_json:JSON.stringify({prompt:'Edit portrait。避免：'+negative,negative:'',steps:0,cfg:0,loras:[]})};
const actions=createResultActions({context:()=>({params,imgId:1}),submitted:()=>null,setRerun:()=>{},backToCanvas:()=>{},capture:()=>({current:()=>true})});
document.querySelector('#restore').onclick=async()=>{await actions.restoreFromResult(window.record);window.restoreCount++;};
window.restoreCount=0;
window.settings=()=>store.peek('settings');
window.setRecord=(record)=>{window.record=record;};
window.setCurrent=(patch)=>store.set({settings:{...store.peek('settings'),...patch}});
window.ready=true;
</script></body></html>`;
const server=http.createServer((req,res)=>{
  if (req.url==='/') {res.setHeader('content-type','text/html; charset=utf-8');return res.end(pageHtml);}
  const file=path.resolve(publicRoot,'.'+new URL(req.url,'http://localhost').pathname);
  if (!file.startsWith(publicRoot+path.sep) || !fs.existsSync(file)) {res.writeHead(404);return res.end();}
  res.setHeader('content-type',file.endsWith('.js')?'text/javascript; charset=utf-8':'application/json; charset=utf-8');
  res.end(fs.readFileSync(file));
});
(async()=>{
  await new Promise(r=>server.listen(0,'127.0.0.1',r));
  let browser;
  try {
    browser=await chromium.launch({headless:true,channel:process.env.SYNCO_BROWSER_CHANNEL||'chrome'});
    const page=await browser.newPage();const errors=[];
    page.on('pageerror',e=>errors.push(e.message));
    await page.goto('http://127.0.0.1:'+server.address().port);
    await page.waitForFunction(()=>window.ready);
    const merged='Edit portrait。避免：'+negative;
    for(let i=0;i<5;i++) {
      await page.click('#restore');
      await page.waitForFunction(n=>window.restoreCount===n,i+1);
      assert.equal(await page.locator('textarea').first().inputValue(),merged);
      const settings=await page.evaluate(()=>window.settings());
      assert.equal(settings.negative,'');assert.equal(settings.steps,24);assert.equal(settings.cfg,4);
      await page.locator('.btn--accent.btn--block').click();
      const payloads=await page.evaluate(()=>window.payloads);
      assert.equal(payloads[i],merged);
      await page.evaluate(prompt=>window.setRecord({id:78,backend:'cloud',settings_json:JSON.stringify({prompt,negative:'',steps:0,cfg:0})}),payloads[i]);
    }
    // Even a record with old duplication/custom prose is preserved verbatim.
    const old=merged+'。避免：'+negative+'。保留手写内容';
    await page.evaluate(prompt=>{window.setCurrent({negative:'different current negative',steps:0,cfg:0});window.setRecord({id:79,backend:'cloud',settings_json:JSON.stringify({prompt})});},old);
    await page.click('#restore');await page.waitForFunction(()=>window.restoreCount===6);
    assert.equal(await page.locator('textarea').first().inputValue(),old);
    assert.equal((await page.evaluate(()=>window.settings())).negative,'');
    assert.equal((await page.evaluate(()=>window.settings())).steps,20);
    await page.evaluate(()=>window.setRecord({id:80,backend:'comfyui',seed:11,settings_json:JSON.stringify({prompt:'local prompt',negative:'local negative',steps:28,cfg:5,loras:[]})}));
    await page.click('#restore');await page.waitForFunction(()=>window.restoreCount===7);
    assert.equal((await page.evaluate(()=>window.settings())).negative,'local negative');
    assert.equal(await page.locator('textarea').first().inputValue(),'local prompt');
    assert.deepEqual(errors,[]);
    console.log('Browser prompt restore passed: five restore/submit cycles, unchanged cloud payload, preserved history text, valid local defaults, local negative restore.');
  } finally {await browser?.close();await new Promise(r=>server.close(r));}
})().catch(e=>{console.error(e);process.exitCode=1;});
