// 文案外置的两本账与一条通道判据：给 lint-layers（当门）与 i18n-check（报进度）共用一份。
//
// 为什么单独一份：两边原来各写各的清单，改文件时只会想起其中一处，
// 于是"门说全绿、进度说还剩一百行"这种自相矛盾的报告就出现了。判据只能有一个出处。
'use strict';

// R6 的账本：前端搬完一个文件就加一行，只增不减。
const DONE_I18N = [
  'public/js/core/i18n.js', 'public/js/shell.js',
  'public/js/core/desktop.js', 'public/js/core/maskEncode.js', 'public/js/core/api.js', 'public/js/core/format.js',
  'public/js/state.js', 'public/js/ui/progress.js', 'public/js/ui/modal.js', 'public/js/views/editor/filmstrip.js',
  'public/js/app.js', 'public/js/gen.js', 'public/js/views/editor/compare.js', 'public/js/views/editor/history.js',
  'public/js/core/theme.js', 'public/js/ui/dialogs.js', 'public/js/ui/phrases.js', 'public/js/ui/backends.js',
  'public/js/ui/settings.js', 'public/js/ui/presets.js', 'public/js/views/home.js',
  'public/js/views/editor/params.js', 'public/js/views/editor/index.js',
  'public/js/views/canvas/index.js', 'public/js/views/project.js', 'public/js/views/editor/adjust.js',
];

// R7 的账本：后端把文案交给字典的文件，同样只增不减。
// 进来的标准是"`#[cfg(test)]` 之前不再有引号里的中文"，不是"这文件我看过"。
//
// 没进账的两类是刻意排除，不是漏了：
//   · `crates/*/src/bin/*.rs`（synco-tools）——双击跑的运维菜单，每条文字都往 stdout 走，
//     读者是坐在机器前的人，不是界面。
//   · `crates/*/examples/*.rs`——对拍用的开发工具，同上。
const DONE_I18N_RS = [
'crates/photoedit-core/src/beauty.rs', 'crates/photoedit-core/src/color.rs', 'crates/photoedit-core/src/face.rs',
  'crates/photoedit-core/src/geometry.rs', 'crates/photoedit-core/src/lib.rs', 'crates/photoedit-core/src/lut.rs',
  'crates/photoedit-core/src/ops.rs', 'crates/photoedit-core/src/pipeline.rs', 'crates/photoedit-core/src/px.rs',
  'crates/photoedit-core/src/warp.rs', 'crates/server/src/api/adjust.rs', 'crates/server/src/api/backends.rs',
  'crates/server/src/api/canvas.rs', 'crates/server/src/api/cloud.rs', 'crates/server/src/api/common.rs',
  'crates/server/src/api/images.rs', 'crates/server/src/api/mod.rs', 'crates/server/src/api/presets.rs',
  'crates/server/src/api/projects.rs', 'crates/server/src/api/results.rs', 'crates/server/src/api/settings.rs',
  'crates/server/src/api/setup.rs', 'crates/server/src/error.rs', 'crates/server/src/img/codec.rs',
  'crates/server/src/img/mod.rs', 'crates/server/src/lib.rs', 'crates/server/src/main.rs',
  'crates/server/src/models/dto.rs', 'crates/server/src/models/entity.rs', 'crates/server/src/models/mod.rs',
  'crates/server/src/repo/adjust.rs', 'crates/server/src/repo/db.rs', 'crates/server/src/repo/images.rs',
  'crates/server/src/repo/mod.rs', 'crates/server/src/repo/presets.rs', 'crates/server/src/repo/projects.rs',
  'crates/server/src/repo/results.rs', 'crates/server/src/repo/settings.rs', 'crates/server/src/service/adjust.rs',
  'crates/server/src/service/backend.rs', 'crates/server/src/service/cloud.rs', 'crates/server/src/service/comfy.rs',
  'crates/server/src/service/face.rs', 'crates/server/src/service/imagesvc.rs', 'crates/server/src/service/mod.rs',
  'crates/server/src/service/queue.rs', 'crates/server/src/service/reclaim.rs', 'crates/server/src/service/refs.rs',
  'crates/server/src/service/releases.rs', 'crates/server/src/service/setup.rs', 'crates/server/src/service/workflow.rs',
  'crates/server/src/state.rs', 'crates/server/src/text.rs', 'crates/server/src/util.rs',
  'crates/server/src/version.rs', 'crates/server/src/web/assets.rs', 'crates/server/src/web/csp.rs',
  'crates/server/src/web/files.rs', 'crates/server/src/web/guard.rs', 'crates/server/src/web/mod.rs',
  'crates/stitch-core/src/buffer.rs', 'crates/stitch-core/src/color.rs', 'crates/stitch-core/src/geom.rs',
  'crates/stitch-core/src/lib.rs', 'crates/stitch-core/src/mask.rs', 'crates/stitch-core/src/par.rs',
  'crates/stitch-core/src/pyramid.rs', 'crates/stitch-core/src/resize.rs', 'crates/stitch-core/src/stitch.rs',
  'src-tauri/build.rs', 'src-tauri/src/main.rs',
];

// 控制台/日志/panic 通道：写给坐在机器前的人，界面读的是 code，两条路不会互相冒充，
// 所以按"走的是哪个通道"划界，而不是在行首撒 i18n-keep。
// 判据是这一行本身在往 stdout/stderr 或 panic 通道里写字——多行宏的续行会漏网，
// 因此这条规则成立的前提是"日志句子写在一行里"。
const CHANNEL = /(^|[^\w.])(print\w*|eprint\w*|panic|unreachable|todo|assert\w*|debug_assert\w*)!|\.expect\s*\(/;

/// 整份都是控制台程序的目录（不进账、也不算待搬）
const isConsoleOnly = rel => /(^|\/)src\/bin\//.test(rel) || /(^|\/)examples\//.test(rel);

/// 这一行算不算"界面要读的文案"：注释与非界面通道都不算
const uiCopy = (line, raw) => !!line.trim() && !/i18n-keep/.test(raw) && !CHANNEL.test(line);

module.exports = { DONE_I18N, DONE_I18N_RS, CHANNEL, isConsoleOnly, uiCopy };
