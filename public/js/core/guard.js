// 「在飞的这一次还算不算屏幕上这张」——异步回来后动屏幕之前的那一道判据。
'use strict';

/**
 * 视图的主体对象是复用的：编辑器换图改 `ctx.imgId`，画布换画布改 `c.imgId`，
 * 本地调整那边 `idOf()` 也是同一个闭包在返回当前那张。所以 `await` 之前抓住的 id，
 * 回来时可能已经属于上一张了——不重新一判，就会把另一张图的返回画到当前这张上。
 *
 * 用法：动作开头 `const same = hold(idOf)`，每个 `await` 之后第一句 `if (!same()) return;`。
 * 这条纪律以前是各站点手写 `if (idOf() !== id) return` 的三个变种，于是同一个仓库里
 * 三种写法并存、漏写的那处没人拦。
 *
 * 它管的是"切走了还回来画"这一种；**A→B→A 回到同一张时它算通过**，那种竞态要的是世代号，
 * 各自的装载里已经有（`canvas` 的 `seq`、`adjust` 的 `seq`/`rev`），别拿这个替。
 *
 * @param {() => any} read 当前主体的标识（id、对象都行，只要换主体时它会变）
 * @returns {() => boolean} 还是抓住那一刻那张吗
 */
export const hold = read => {
  const at = read();
  return () => read() === at;
};
