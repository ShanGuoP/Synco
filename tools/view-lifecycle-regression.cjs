'use strict';
// 执行实际入口，控制网络时序；这是受控执行验证，不是浏览器端到端测试。
const fs = require('fs'), path = require('path'), vm = require('vm'), assert = require('assert/strict');
const settle = () => new Promise(setImmediate);
const read = rel => fs.readFileSync(path.join(__dirname, '..', rel), 'utf8')
  .replace(/^import .*;\r?$/gm, '').replace(/^export default .*;\r?$/gm, '').replace(/^export /gm, '');

function fixture(kind) {
  const host = { hidden: true, classList: { remove() {} } }, shell = { style: { visibility: '' } };
  const requests = [], loads = [], errors = [], navigations = [];
  let generation = 0;
  const fake = {
    painter: { flush: async () => {} }, adjust: { exit() {}, flush: async () => {} },
    params: { sync() {} }, compare: { hide() {} },
  };
  const scope = {
    console, AbortController, fake,
    routeGen: () => generation,
    $: selector => selector === '#shell' ? shell : host,
    store: { get: () => ({ project: null, images: [] }), peek: () => null },
    loadProject: (id, current) => new Promise((resolve, reject) => requests.push({ id, current, resolve, reject })),
    loadPhrases: async () => {},
    window: { addEventListener() {} }, beforeSwitch() {},
    toastErr: (...args) => errors.push(args), t: key => key, go: p => navigations.push(p),
    probeLoad: async id => loads.push(id),
  };
  vm.createContext(scope);
  vm.runInContext(read('public/js/core/viewSession.js'), scope);
  const isEditor = kind === 'editor';
  vm.runInContext(read(`public/js/views/${kind}/index.js`) + (isEditor
    ? '\nctx=fake; showImage=probeLoad; globalThis.openProbe=openEditor; globalThis.closeProbe=closeEditor;'
    : '\nc=fake; load=probeLoad; paintChips=()=>{}; globalThis.openProbe=openCanvas; globalThis.closeProbe=closeCanvas;'), scope);
  return { host, requests, loads, errors, navigations, fake, open: scope.openProbe, close: scope.closeProbe,
    route: () => ++generation };
}

(async () => {
  for (const kind of ['editor', 'canvas']) {
    // 关闭后迟到的成功与失败均不能打开旧覆盖层、显示错误或导航。
    for (const fails of [false, true]) {
      const f = fixture(kind), opening = f.open(1, 1);
      await settle();
      f.close();
      assert.equal(f.requests[0].current(), false);
      fails ? f.requests[0].reject(new Error('old load')) : f.requests[0].resolve({});
      await opening;
      assert.equal(f.host.hidden, true);
      assert.deepEqual(f.loads, []);
      assert.deepEqual(f.errors, []);
      assert.deepEqual(f.navigations, []);
    }
    // A→B→A：后一次 A 到达后，前两次的响应都不能覆盖它。
    const f = fixture(kind);
    const a = f.open(1, 10); await settle(); f.route();
    const b = f.open(2, 20); await settle(); f.route();
    const again = f.open(1, 11);
    await settle();
    f.requests[2].resolve({}); await again;
    f.requests[1].resolve({}); await b;
    f.requests[0].resolve({}); await a;
    assert.deepEqual(f.loads, [11]);
    assert.equal(f.host.hidden, false);
    // 路由世代变化本身即可使当前会话失效。
    const routed = fixture(kind), pending = routed.open(1, 1);
    await settle();
    routed.route(); routed.requests[0].resolve({}); await pending;
    assert.equal(routed.host.hidden, true);
    const saving = fixture(kind);
    let saved;
    saving.fake.painter.flush = () => new Promise(r => saved = r);
    const waitingSave = saving.open(1, 1);
    // close 也会 flush；避免把关闭产生的第二份 promise 当成打开时那份。
    saving.fake.painter.flush = async () => {};
    saving.close(); saved(); await waitingSave;
    assert.equal(saving.host.hidden, true);
    assert.deepEqual(saving.requests, []);
    console.log(`${kind}: close/success, close/error, A→B→A, route invalidation, leaving while save is pending passed`);
  }
  // 项目详情返回后还会 await cfg；此处离开同样不得覆盖新会话的项目数据。
  const writes = [], scope = { console, t: s => s, routeGen: () => generation,
    api: { project: async () => ({ project: { id: 1 }, images: [] }), cfg: () => new Promise(r => cfgDone = r) },
    createStore: () => ({ peek: () => null, set: (obj, key) => writes.push({ obj, key }) }) };
  let generation = 0, cfgDone;
  vm.createContext(scope);
  vm.runInContext(read('public/js/state.js') + '\nglobalThis.loadProbe=loadProject;', scope);
  const pending = scope.loadProbe(1);
  await new Promise(setImmediate);
  generation++; cfgDone({}); await pending;
  assert.equal(writes.some(w => w.key === 'project'), false);
  console.log('project: leaving while cfg is pending passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
