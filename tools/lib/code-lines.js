// 注释/字符串扫描器：给 lint-layers 与 i18n-check 共用的一份"这行算不算代码"的判据。
//
// 为什么单独一份：两个工具原来各写了一遍"只看 /* 和 */ 的字面出现"，于是
// `accept: 'image/*'` 里那个斜杠星被当成块注释开头，从那一行起到下一个 `*/` 为止
// 整段被当注释跳过——中文有没有漏、层边界有没有破，判据当场失明了一整片。
// 现在按字符走状态机：串里的注释符号不算数，注释里的引号也不算数。
'use strict';
const fs = require('fs');

/**
 * 返回 [{ n, raw, line }]：raw 是原行（留着看 i18n-keep 这种行尾标记），
 * line 是剥掉注释之后的行——判"这行是不是代码"只看它。
 */
function codeLines(file, { html = false } = {}) {
  return scan(fs.readFileSync(file, 'utf8'), html, file.endsWith('.rs'));
}

function scan(src, html, rust) {
  // HTML 注释与 // 同地位：解释不是文案。整块先摘掉（行数不变，行号才对得上）
  if (html) src = src.replace(/<!--[\s\S]*?-->/g, m => m.replace(/[^\n]/g, ' '));
  const rows = [];
  let inBlock = false, inLine = false, quote = null, depth = 0;
  let rawHashes = -1;                   // >=0 表示正在 Rust 的 r#"…"# 原始串里，收尾要连着那么多个 #
  const tpl = [];                       // 每个 ${ 记下它左花括号所在的层级，配平的 } 才退出模板
  let text = '', raw = '';
  const flush = () => { rows.push({ n: rows.length + 1, raw, line: text }); text = ''; raw = ''; };

  for (let i = 0; i < src.length; i++) {
    const c = src[i], nx = src[i + 1];
    raw += c;

    if (c === '\n') { inLine = false; flush(); continue; }
    if (inLine) continue;

    if (inBlock) {
      if (c === '*' && nx === '/') { i++; raw += nx; text += '  '; inBlock = false; }
      continue;
    }

    if (quote) {
      text += c;
      // 原始串里反斜杠不转义，收尾的引号还得连着那么多个 #
      if (rawHashes >= 0) {
        if (c === quote && src.substr(i + 1, rawHashes) === '#'.repeat(rawHashes)) {
          for (let h = 0; h < rawHashes; h++) { i++; raw += '#'; }
          quote = null; rawHashes = -1;
        }
        continue;
      }
      if (c === '\\') { const e = src[i + 1] || ''; if (e) { i++; raw += e; text += e; } continue; }
      if (c === quote) { quote = null; continue; }
      // 模板串里的 ${…} 是代码：退出字符串状态，配平的 } 再把它收回来
      if (quote === '`' && c === '$' && nx === '{') {
        i++; raw += nx; text += nx; quote = null; depth++; tpl.push(depth);
        continue;
      }
      continue;
    }

    if (c === '/' && nx === '/') { i++; raw += nx; inLine = true; continue; }
    if (c === '/' && nx === '*') { i++; raw += nx; inBlock = true; continue; }
    if (c === '"' || c === '`') {
      // r"…" / r#"…"：Rust 的原始串（建表 SQL、装机脚本正文都这么写）。不认出来，
      // 串里的 `"` 会提前收尾，剩下的部分就被当成代码乱判一通。
      if (rust && c === '"' && src[i - 1] === 'r') {
        let h = 0;
        while (src[i + 1 + h] === '#') h++;
        if (h) { rawHashes = h; quote = '"'; text += c; for (let k = 0; k < h; k++) { i++; raw += '#'; } continue; }
      }
      quote = c; text += c; continue;
    }
    if (c === "'") {
      if (!rust) { quote = c; text += c; continue; }
      // Rust 的单引号大半是生命周期（`&'static str`、`impl FnOnce() + 'static`），
      // 当字符串开头会把后面整片的注释与代码一起吞进"串"里——判据就此失明。
      // 只有正好包住一个字符（或一个转义）的 `'x'` / `'\n'` 才是字面量，其余当普通字符走过去。
      const close = src[i + 1] === '\\' ? i + 3 : i + 2;
      if (src[close] === "'") {
        for (let k = i + 1; k <= close; k++) { raw += src[k]; text += src[k]; }
        i = close;
      } else {
        text += c;
      }
      continue;
    }
    if (c === '{') { depth++; text += c; continue; }
    if (c === '}') {
      depth--;
      if (tpl.length && depth < tpl[tpl.length - 1]) { tpl.pop(); quote = '`'; }
      text += c;
      continue;
    }
    text += c;
  }
  flush();
  return rows;
}

module.exports = { codeLines, scan };
