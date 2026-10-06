// 极简状态容器：不可变快照 + 订阅，无依赖无魔法
'use strict';

export function createStore(initial = {}) {
  let state = { ...initial };
  const subs = new Set();

  const notify = key => { for (const fn of subs) { try { fn(state, key); } catch (e) { console.error(e); } } };

  return {
    get: () => state,
    peek: k => state[k],

    /** patch 可以是对象或 (state)=>部分对象；返回合并后的新状态 */
    set(patch, key) {
      const next = typeof patch === 'function' ? patch(state) : patch;
      if (!next) return state;
      let changed = false;
      for (const [k, v] of Object.entries(next)) if (state[k] !== v) { changed = true; break; }
      if (!changed) return state;
      state = { ...state, ...next };
      notify(key);
      return state;
    },

    /** 集合类字段（Set）的显式变更入口，避免就地改导致订阅方拿到同一引用 */
    replace(k, v) { return this.set({ [k]: v }, k); },

    subscribe(fn) { subs.add(fn); return () => subs.delete(fn); },
  };
}
