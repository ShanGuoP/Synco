// 空状态：居中美化插画，不再是一句灰字
'use strict';
import { el } from '../core/dom.js';

/* 纸面上走墨线：主体用 currentColor（= --txt），专色只点一处 */
const ART = {
  photos: `<svg class="empty__art" viewBox="0 0 132 96" fill="none">
    <rect x="24" y="20" width="76" height="56" stroke="currentColor" stroke-opacity=".18" stroke-width="1.2" transform="rotate(-7 62 48)"/>
    <rect x="32" y="24" width="76" height="56" fill="var(--bg-card)" stroke="currentColor" stroke-opacity=".3" stroke-width="1.2" transform="rotate(4 70 52)"/>
    <rect x="38" y="27" width="66" height="48" fill="var(--bg-card)" stroke="currentColor" stroke-opacity=".5" stroke-width="1.2"/>
    <path d="M44 66l14-15 10 10 9-9 15 14" stroke="currentColor" stroke-opacity=".45" stroke-width="1.6"/>
    <circle cx="55" cy="41" r="4.5" fill="var(--spot)" fill-opacity=".75"/>
    <rect x="96" y="57" width="26" height="26" fill="var(--spot)"/>
    <path d="M109 63v14M102 70h14" stroke="var(--txt-inv)" stroke-width="2"/>
  </svg>`,
  folder: `<svg class="empty__art" viewBox="0 0 132 96" fill="none">
    <path d="M22 34h88v44H22z" fill="var(--bg-card)" stroke="currentColor" stroke-opacity=".45" stroke-width="1.4"/>
    <path d="M22 34l6-8h22l8 8" stroke="currentColor" stroke-opacity=".45" stroke-width="1.4"/>
    <path d="M22 44h88" stroke="currentColor" stroke-opacity=".2" stroke-width="1.2"/>
    <rect x="48" y="16" width="38" height="26" fill="var(--bg-card)" stroke="currentColor" stroke-opacity=".3" stroke-width="1.2" transform="rotate(-6 67 29)"/>
    <rect x="54" y="52" width="24" height="24" fill="var(--spot)"/>
    <path d="M66 58v12M60 64h12" stroke="var(--txt-inv)" stroke-width="2"/>
  </svg>`,
  brush: `<svg class="empty__art" viewBox="0 0 132 96" fill="none">
    <rect x="30" y="18" width="72" height="60" fill="var(--bg-card)" stroke="currentColor" stroke-opacity=".45" stroke-width="1.4"/>
    <path d="M44 62c8-14 20-6 26-16s14-4 18 2" stroke="var(--spot)" stroke-width="1.8" stroke-dasharray="4 5"/>
    <circle cx="52" cy="40" r="3" fill="var(--spot)" fill-opacity=".6"/><circle cx="70" cy="34" r="2" fill="var(--spot)" fill-opacity=".4"/>
    <path d="m84 44 14-14 8 8-14 14z" fill="var(--bg-card)" stroke="currentColor" stroke-opacity=".55" stroke-width="1.3"/>
    <path d="M84 44 74 62l10-4z" fill="var(--spot)"/>
  </svg>`,
  canvas: `<svg class="empty__art" viewBox="0 0 132 96" fill="none">
    <rect x="20" y="14" width="70" height="56" fill="var(--bg-card)" stroke="currentColor" stroke-opacity=".45" stroke-width="1.4"/>
    <path d="M28 58c10-16 18-5 26-16s14-6 20 3" stroke="currentColor" stroke-opacity=".4" stroke-width="1.6" stroke-dasharray="4 5"/>
    <circle cx="40" cy="30" r="5.5" stroke="currentColor" stroke-opacity=".38" stroke-width="1.3"/>
    <circle cx="57" cy="25" r="4" stroke="currentColor" stroke-opacity=".28" stroke-width="1.3"/>
    <path d="M20 70h70" stroke="currentColor" stroke-opacity=".2" stroke-width="1.2"/>
    <rect x="86" y="46" width="26" height="26" fill="var(--spot)"/>
    <path d="M99 52v14M92 59h14" stroke="var(--txt-inv)" stroke-width="2"/>
  </svg>`,
  compare: `<svg class="empty__art" viewBox="0 0 132 96" fill="none">
    <rect x="26" y="20" width="40" height="56" fill="var(--bg-card)" stroke="currentColor" stroke-opacity=".4" stroke-width="1.3"/>
    <rect x="66" y="20" width="40" height="56" fill="var(--bg-card)" stroke="var(--spot)" stroke-opacity=".6" stroke-width="1.3"/>
    <path d="M66 14v68" stroke="currentColor" stroke-opacity=".55" stroke-width="1.4" stroke-dasharray="3 4"/>
    <rect x="56" y="38" width="20" height="20" fill="var(--spot)"/>
    <path d="m63 48-3-3M63 48l3-3M69 48l3-3M69 48l-3-3" stroke="var(--txt-inv)" stroke-width="1.6"/>
  </svg>`,
};

export function emptyState(kind, title, desc, action) {
  return el('div.empty', {},
    el('div', { html: ART[kind] || ART.photos }),
    el('div.empty__t', { text: title }),
    desc ? el('div.empty__d', { text: desc }) : null,
    action || null,
  );
}
