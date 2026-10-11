// 覆盖层复用 DOM，但每次打开都属于独立会话；关闭和换路由均让旧异步工作失效。
import { routeGen } from './router.js';

export function createViewSession() {
  let active = null;
  return {
    begin() {
      active?.abort();
      const controller = active = new AbortController();
      const generation = routeGen();
      const current = () => active === controller && !controller.signal.aborted && generation === routeGen();
      controller.current = current;
      return { current, signal: controller.signal };
    },
    end() { active?.abort(); active = null; },
    current() { return active?.current() || false; },
  };
}
