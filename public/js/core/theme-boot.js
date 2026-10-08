// 首帧之前把底色定下来：等 app.js 那个模块跑起来再上色，深色档下会先闪一下纸白。
// 所以它是普通脚本而不是 module——module 默认 defer，赶不上首帧。
// 读写的就是 core/theme.js 那一个键与那一个 data-theme，模块跑起来会接着把它对齐。
try {
  var m = localStorage.getItem('synco.theme') || 'system';
  document.documentElement.dataset.theme =
    m === 'light' || m === 'dark' ? m
      : (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light');
} catch (e) { /* 读不到就按纸白走，:root 本身就是亮色 */ }
