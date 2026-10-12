// 历史结果恢复、派生、删除与导出；通过入口回调读写编辑会话。
import { api } from '../../../core/api.js';
import { go } from '../../../core/router.js';
import { store, loadProject, diffSettings, defaultsFromCfg } from '../../../state.js';
import { toastOk, toastErr, toastBusy } from '../../../ui/toast.js';
import { confirm } from '../../../ui/modal.js';
import { settingsModal } from '../../../ui/settings.js';
import { dx, t } from '../../../core/i18n.js';
import { SEED_MAX } from '../params.js';
const parseSettings = r => { try { return JSON.parse(r.settings_json || 'null'); } catch { return null; } };

/** 一条结果用什么名义展示：云端没有步数/CFG/种子，别说本机那套话（批量那条的快照里没有 cloud 节） */
export const resultTag = r => {
  const c = parseSettings(r)?.cloud;
  return r.backend === 'cloud'
    ? [c?.model, c?.quality, c?.size].filter(Boolean).join(' · ') || t('pm.modeCloud')
    : t('ed.resultTag', { steps: r.steps, cfg: r.cfg, seed: r.seed });
};


export function createResultActions({ context, submitted, setRerun, backToCanvas, syncExport, aliveDone, capture }) {
  /** 把某条成图复制成项目里的一张新图，并跳过去在它上面涂 */
  /** 「本地调整 → 应用为新图」之后：项目要重取（多了一行），并跳到那张新图上 */
  async function forkAdjusted(imageId) {
    const action = capture();
    if (!imageId) return;
    const busy = toastBusy(t('ed.registering'));
    try {
      await loadProject(action.projectId, action.current);
      if (!action.current()) { busy.close(); return; }
      busy.close();
      go(`/p/${action.projectId}/e/${imageId}`);
    } catch (e) { busy.close(); toastErr(t('ed.newImageFail'), e.message); }
  }

  async function forkResult(r) {
    const action = capture();
    if (!r?.final_url) { toastErr(t('ed.noResultYet')); return; }
    if (r.final_dead) { toastErr(t('ed.resultGone'), t('ed.resultGoneBody')); return; }
    const busy = toastBusy(t('ed.copying'));
    try {
      const f = await api.forkResult(r.id);
      if (!action.current()) { busy.close(); return; }
      await loadProject(action.projectId, action.current);
      if (!action.current()) { busy.close(); return; }
      busy.close();
      go(`/p/${action.projectId}/e/${f.image_id}`);
      toastOk(t('ed.forked'), t('ed.forkedBody', { name: f.name }));
    } catch (e) { busy.close(); toastErr(t('ed.forkFail'), e.message); }
  }

  /** 删一条生成记录：连着它的成图文件一起删，原图与遮罩不动 */
  async function delResult(r) {
    const action = capture();
    const ok = await confirm({
      title: t('ed.delRecTitle', { id: r.id }),
      text: r.final_url ? t('ed.delRecText') : t('ed.delRecOnly'),
      danger: true, okLabel: t('ed.delRecBtn'),
    });
    if (!ok || !action.current()) return;
    try {
      await api.delResult(r.id);
      if (!action.current()) return;
      if (context().cmpId === r.id) context().compare.hide();
      const info = await api.image(action.imgId);
      if (!action.current()) return;
      context().info = info;
      context().history.setResults(info.results, info.last?.status === 'done' ? info.last.id : null);
      syncExport();
      toastOk(t('ed.recDeleted'), `#${r.id}`);
    } catch (e) { toastErr(t('ed.delFail'), e.message); }
  }

  /** 把某条结果当时的参数搬回面板，等用户自己点提交；种子默认沿用 */
  async function restoreFromResult(r) {
    const action = capture();
    const s = parseSettings(r);
    if (!s) { toastErr(t('ed.noParams'), t('ed.noParamsWhy')); return; }
    const cur = store.peek('settings') || {};
    const dirty = submitted() && diffSettings(submitted(), cur).length;
    if (dirty) {
      const ok = await confirm({
        title: t('ed.restoreTitle'),
        text: t('ed.restoreText', { id: r.id }),
        okLabel: t('ed.restoreBtn'),
      });
      if (!ok || !action.current()) return;
    }
    /* 云端记录没有步数/LoRA/种子可回填，正向里还并进了当初的负面，所以只搬指令、不动本地那几行。
       注意必须把 steps/cfg/loras 按**本机这一路**的合法值重取：云端行是 0 步 0 CFG 的形状，
       整份搬进面板后切回本机再点提交，就把 0 直接送进了 KSampler。 */
    if (r.backend === 'cloud') {
      const d = defaultsFromCfg(store.peek('cfg') || {});
      const legal = (v, lo, hi) => (Number(v) >= lo && Number(v) <= hi ? Number(v) : null);
      const next = {
        ...s,
        prompt: s.prompt || '',
        negative: cur.negative || d.negative,
        steps: legal(cur.steps, 4, 60) ?? d.steps,
        cfg: legal(cur.cfg, 0.5, 14) ?? d.cfg,
        loras: (cur.loras || []).length ? cur.loras : d.loras,
        seed: cur.seed || 0,
        randomSeed: cur.randomSeed !== false,
      };
      setRerun(r.id);
      context().params.applySettings(next, [...diffSettings(cur, next), 'prompt']);
      context().params.line(t('ed.restoredCloud', { id: r.id }));
      toastOk(t('ed.restored'), t('ed.restoredBody', { tag: resultTag(r) }));
      backToCanvas();
      return;
    }
    /* 提交时后端按「面板种子 + 图片序号」取模下发，回填要先把这张图的序号减回去，
       否则重跑用的是另一个种子，"改自 #id" 的复现承诺就破了。 */
    const raw = Number(r.seed ?? s.seed ?? 0);
    const base = (((raw - context().imgId) % SEED_MAX) + SEED_MAX) % SEED_MAX;
    const next = { ...s, seed: base, randomSeed: false };
    setRerun(r.id);
    context().params.applySettings(next, [...diffSettings(cur, next), 'seed', 'randomSeed']);
    context().params.line(t('ed.restoredWf', { id: r.id }));
    toastOk(t('ed.restored'), (base + context().imgId) % SEED_MAX === raw
      ? t('ed.seedKept', { id: r.id, seed: base })
      : t('ed.seedOut', { id: r.id }));
    backToCanvas();
  }

  /** 导出：成图直接复制到设置里的本机文件夹；没配目录就带去设置页 */
  async function exportCurrent() {
    const action = capture();
    const done = aliveDone();
    if (!done) { toastErr(t('ed.noExport')); return; }
    const busy = toastBusy(t('ed.exporting'));
    try {
      const r = await api.exportRun([done.id]);
      busy.close();
      const one = r.files?.[0];
      if (!one || one.skipped) { toastErr(t('ed.exportFail'), one?.reason || t('ed.unknown')); return; }
      // 重算不成时交出去的是 q95 那一档（后缀也跟着变成 .jpg）——必须说出来，
      // 不能让人以为手里那张与屏幕上看到的是同一份保真度
      const note = one.lossless === false ? ` · ${dx(one.why || 'srv.lossless.noSnap')}` : '';
      toastOk(t('ed.exported'), `${r.dir}\\${one.file}${note}`);
    } catch (e) {
      busy.close();
      const m = String(e.message || e);
      if (e.code === 'srv.settings.exportUnset') {
        toastErr(t('ed.exportUnset'), t('ed.exportUnsetBody'));
        settingsModal('export');
      } else toastErr(t('ed.exportFail'), m);
    }
  }


  return { forkAdjusted, forkResult, delResult, restoreFromResult, exportCurrent };
}
