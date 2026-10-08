// 首帧之前把底色定下来：等 app.js 那个模块跑起来再上色，深色档下会先闪一下纸白。
// 所以它是普通脚本而不是 module——module 默认 defer，赶不上首帧。
// 读写的就是 core/theme.js 那一个键与那一个 data-theme，模块跑起来会接着把它对齐。
// 「减弱动效」也在这里挂：body 这会儿还不存在，所以挂 <html>（core/motion.js 之后会把两处对齐），
// 不挂的话加载用的那几条动画会在第一帧被看见一次——减弱动效的人要的正是"从头到尾没有动"。
try {
  var m = localStorage.getItem('synco.theme') || 'system';
  document.documentElement.dataset.theme =
    m === 'light' || m === 'dark' ? m
      : (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light');
} catch (e) { /* 读不到就按纸白走，:root 本身就是亮色 */ }
try {
  if (localStorage.getItem('synco.motion') === '1' ||
      matchMedia('(prefers-reduced-motion: reduce)').matches) {
    document.documentElement.classList.add('reduce-motion');
  }
} catch (e) { /* 读不到就按"能动"走，模块加载后 motion.js 会再判一次 */ }
