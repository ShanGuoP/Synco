// Source-domain mask editing and white-balance picking; never paints the AI redraw mask.
import { el, clamp } from '../../../core/dom.js';
export function makeLocalOverlay({ layer, viewport, modeOf, readyOf, hoverReady, moveReady, getOps, getInfo, getLocal, onChange, onPick }) {
  const canvas = el('canvas.develop-mask-overlay', { width: 1000, height: 1000, hidden: true });
  const hoverCanvas = el('canvas.develop-mask-hover', { hidden: true, 'aria-hidden': 'true' });
  layer.append(canvas, hoverCanvas);
  let drag = null;
  let move = null;
  let hovered = null, hoverFrame = 0;
  function moveCursor(value) { layer.style.cursor=value; layer.classList.toggle('is-moving-local-mask',!!value); }
  const layerPoint = e => { const r = layer.getBoundingClientRect(); return [(e.clientX-r.left)/Math.max(1,r.width), (e.clientY-r.top)/Math.max(1,r.height)]; };
  // Reverse the photo's crop/rotation/flips before translating source-domain masks.
  function sourceProjection() {
    const g = getOps().geometry, info = getInfo();
    let w = info.w, h = info.h;
    const turns = Math.round(g.rotate_deg/90), q = ((turns%4)+4)%4;
    if (q%2) [w,h]=[h,w];
    const angle = (g.rotate_deg-turns*90)*Math.PI/180, c=Math.cos(angle), s=Math.sin(angle);
    const rw=Math.ceil(w*Math.abs(c)+h*Math.abs(s)), rh=Math.ceil(w*Math.abs(s)+h*Math.abs(c));
    const crop=g.crop || [0,0,1,1], ox=Math.round(crop[0]*rw), oy=Math.round(crop[1]*rh);
    const cw=Math.max(1,Math.round(Math.min(1,crop[0]+crop[2])*rw)-ox), ch=Math.max(1,Math.round(Math.min(1,crop[1]+crop[3])*rh)-oy);
    const sourceW=info.w, sourceH=info.h, distortion=getOps().develop.lens.distortion/100;
    return p => {
    let x=p[0]*cw, y=p[1]*ch, sw=w, sh=h;
    const dx=x-(cw-1)/2, dy=y-(ch-1)/2, scale=Math.max((cw-1)/2,(ch-1)/2,1);
    const f=1+distortion*.25*(dx*dx+dy*dy)/(scale*scale);
    x=(cw-1)/2+dx*f+ox-(rw-1)/2; y=(ch-1)/2+dy*f+oy-(rh-1)/2;
    [x,y]=[c*x+s*y+(w-1)/2,-s*x+c*y+(h-1)/2];
    for(let i=0;i<q;i++){[x,y]=[y,sw-1-x];[sw,sh]=[sh,sw];}
    if(g.flip_h)x=sw-1-x; if(g.flip_v)y=sh-1-y;
    return [x/sourceW,y/sourceH];
    };
  }
  const sourcePoint = e => sourceProjection()(layerPoint(e));
  function showHover(local) {
    if(hovered===local)return;
    hovered=local;
    if(!local){hoverCanvas.hidden=true;return;}
    queueHover();
  }
  function queueHover() { if(!hoverFrame)hoverFrame=requestAnimationFrame(()=>{hoverFrame=0;sync();}); }
  function distance(p,a,b,aspect=1) {
    const dx=(b[0]-a[0])*aspect,dy=b[1]-a[1];
    const t=clamp(((p[0]-a[0])*aspect*dx+(p[1]-a[1])*dy)/(dx*dx+dy*dy || 1),0,1);
    return Math.hypot((p[0]-a[0])*aspect-dx*t,p[1]-a[1]-dy*t);
  }
  function hit(p,m) {
    let inside=false;
    if(m.kind==='radial')inside=((p[0]-m.center[0])/m.radius[0])**2+((p[1]-m.center[1])/m.radius[1])**2<=1;
    if(m.kind==='linear')inside=distance(p,m.center,m.end)<.04;
    const info=getInfo(), aspect=info.w/info.h;
    for(const stroke of m.strokes){let ink=false;for(let i=0;i<stroke.points.length;i++)if(distance(p,stroke.points[i],stroke.points[Math.min(i+1,stroke.points.length-1)],aspect)<=stroke.radius*Math.sqrt(aspect)){ink=true;break;}if(ink)inside=!stroke.erase;}
    return inside;
  }
  const canMove = () => modeOf()==='off' && moveReady() && !layer.classList.contains('pan-through');
  layer.addEventListener('pointerdown', e => {
    if(e.button!==0 || !canMove())return;
    const local=getLocal(), p=sourcePoint(e); if(!local || !hit(p,local.mask))return;
    e.preventDefault(); e.stopImmediatePropagation();
    move={start:p,local,mask:structuredClone(local.mask),pointer:e.pointerId,changed:false};
    layer.setPointerCapture(e.pointerId); moveCursor('grabbing'); showHover(local);
  },true);
  layer.addEventListener('pointermove', e => {
    if(!move){const local=modeOf()==='off' && hoverReady() && !layer.classList.contains('pan-through') && getLocal(), over=local && e.pointerType!=='touch' && hit(sourcePoint(e),local.mask);moveCursor(over && canMove()?'grab':'');showHover(over?local:null);return;}
    if(e.pointerId!==move.pointer || move.local!==getLocal())return;
    e.preventDefault(); e.stopImmediatePropagation();
    const p=sourcePoint(e), original=move.mask;
    const points=original.kind==='brush'?original.strokes.flatMap(s=>s.points):original.kind==='linear'?[original.center,original.end]:[original.center];
    const delta=p.map((v,a)=>clamp(v-move.start[a],-Math.min(...points.map(p=>p[a])),1-Math.max(...points.map(p=>p[a]))));
    const translate=p=>p.map((v,a)=>v+delta[a]);
    const m=move.local.mask;
    m.center=translate(original.center); m.end=original.kind==='linear'?translate(original.end):original.end;
    m.strokes=original.strokes.map(s=>({...s,points:s.points.map(translate)}));
    move.changed=delta.some(v=>Math.abs(v)>1e-6);
    queueHover();
  },true);
  function finishMove(e) {
    if(!move || e.pointerId!==move.pointer)return;
    e.stopImmediatePropagation();const saved=move;move=null;moveCursor('');
    if(e.type==='pointercancel')saved.local.mask=saved.mask;
    else if(saved.changed)onChange();
    if(e.type==='pointercancel')showHover(null);
    else { const p=layerPoint(e);showHover(p.every(v=>v>=0 && v<=1)?saved.local:null);queueHover(); }
  }
  for(const event of ['pointerup','pointercancel','lostpointercapture'])layer.addEventListener(event,finishMove,true);
  layer.addEventListener('pointerleave',()=>{if(!move){moveCursor('');showHover(null);}});
  const point = e => { const r = canvas.getBoundingClientRect(); return [clamp((e.clientX - r.left) / Math.max(1, r.width), 0, 1), clamp((e.clientY - r.top) / Math.max(1, r.height), 0, 1)]; };
  canvas.addEventListener('pointerdown', e => {
    if (e.button !== 0 || viewport.mode === 'pan') return;
    if (modeOf() === 'white') { e.preventDefault(); e.stopPropagation(); onPick(point(e)); return; }
    const local = getLocal(); if (!local) return;
    e.preventDefault(); e.stopPropagation(); canvas.setPointerCapture(e.pointerId);
    const p = point(e), m = local.mask;
    drag = { start: p, local };
    if (m.kind === 'brush') { if (m.strokes.length >= 128 || m.strokes.reduce((n,s) => n+s.points.length, 0) >= 4096) { drag = null; return; } m.strokes.push({ points: [p], radius: Math.max(0.002, m.radius[0] * 0.2), erase: e.shiftKey }); }
    else { m.center = p; m.end = [Math.min(1, p[0] + 0.005), p[1]]; }
    sync();
  });
  canvas.addEventListener('pointermove', e => {
    if (!drag || drag.local !== getLocal()) return;
    const p = point(e), m = drag.local.mask;
    if (m.kind === 'brush') { const s = m.strokes.at(-1), last = s.points.at(-1); if (Math.hypot(p[0] - last[0], p[1] - last[1]) > 0.003 && m.strokes.reduce((n,s) => n+s.points.length, 0) < 4096) s.points.push(p); }
    else if (m.kind === 'linear') m.end = p;
    else m.radius = [Math.max(0.005, Math.abs(p[0] - m.center[0])), Math.max(0.005, Math.abs(p[1] - m.center[1]))];
    sync();
  });
  const end = () => { if (!drag) return; drag = null; onChange(); };
  for (const event of ['pointerup', 'pointercancel', 'lostpointercapture']) canvas.addEventListener(event, end);
  function sync() {
    if(move && move.local!==getLocal()){move.local.mask=move.mask;move=null;moveCursor('');}
    if(!canMove())moveCursor('');
    if(hovered!==getLocal() || modeOf()!=='off' || !hoverReady())showHover(null);
    canvas.hidden = !['local', 'white'].includes(modeOf()) || !readyOf();
    hoverCanvas.hidden=!hovered;
    const info=getInfo(), rect = layer.getBoundingClientRect(), aspect = canvas.hidden && hovered ? info.w/info.h : rect.width / Math.max(1, rect.height);
    const w = Math.max(1, Math.round(aspect > 1 ? 1000 : 1000 * aspect)), h = Math.max(1, Math.round(aspect > 1 ? 1000 / aspect : 1000));
    canvas.width = w; canvas.height = h;
    const c = canvas.getContext('2d'); c.clearRect(0, 0, w, h);
    if (canvas.hidden && !hovered || modeOf() === 'white') return;
    const m = getLocal()?.mask; if (!m) return;
    const x = m.center[0] * w, y = m.center[1] * h; c.fillStyle = 'rgba(210,80,80,.25)'; c.strokeStyle = 'rgba(255,255,255,.9)'; c.lineWidth = 2;
    if (m.kind === 'radial') { c.beginPath(); c.ellipse(x, y, m.radius[0] * w, m.radius[1] * h, 0, 0, Math.PI * 2); c.fill(); c.stroke(); }
    if (m.kind === 'linear') { const ex = m.end[0] * w, ey = m.end[1] * h; if (Math.hypot(ex-x,ey-y)>0.1) { const g = c.createLinearGradient(x,y,ex,ey); g.addColorStop(0,'rgba(210,80,80,0)'); g.addColorStop(1,'rgba(210,80,80,.3)'); c.fillStyle=g; c.fillRect(0,0,w,h); } c.beginPath();c.moveTo(x,y);c.lineTo(ex,ey);c.stroke(); }
    for (const s of m.strokes) { c.globalCompositeOperation=s.erase?'destination-out':'source-over';c.strokeStyle='rgba(210,80,80,.3)';c.lineWidth=s.radius*2*Math.sqrt(w*h);c.lineCap='round';c.lineJoin='round';c.beginPath(); for (const [i,p] of s.points.entries()) { if (!i) c.moveTo(p[0]*w,p[1]*h); else c.lineTo(p[0]*w,p[1]*h); } if(s.points.length===1)c.lineTo(s.points[0][0]*w+0.01,s.points[0][1]*h);c.stroke(); }
    c.globalCompositeOperation='source-over';
    if(hovered) {
      // Sample the source overlay through the same inverse geometry used for dragging.
      const affine=!getOps().develop.lens.distortion, edge=affine?1000:384, ratio=rect.width/Math.max(1,rect.height);
      hoverCanvas.width=Math.max(1,Math.round(ratio>1?edge:edge*ratio));hoverCanvas.height=Math.max(1,Math.round(ratio>1?edge/ratio:edge));
      const hc=hoverCanvas.getContext('2d'), project=sourceProjection();
      if(affine) {
        const o=project([0,0]),px=project([1,0]),py=project([0,1]);
        const a=px[0]-o[0],b=px[1]-o[1],cc=py[0]-o[0],d=py[1]-o[1],det=a*d-b*cc;
        const ow=hoverCanvas.width,oh=hoverCanvas.height;
        hc.setTransform(ow*d/(det*w),-oh*b/(det*w),-ow*cc/(det*h),oh*a/(det*h),ow*(cc*o[1]-d*o[0])/det,oh*(b*o[0]-a*o[1])/det);
        hc.drawImage(canvas,0,0);return;
      }
      const out=hc.createImageData(hoverCanvas.width,hoverCanvas.height), source=c.getImageData(0,0,w,h).data;
      for(let y=0;y<hoverCanvas.height;y++)for(let x=0;x<hoverCanvas.width;x++) {
        const p=project([(x+.5)/hoverCanvas.width,(y+.5)/hoverCanvas.height]);
        const sx=Math.floor(p[0]*w),sy=Math.floor(p[1]*h);if(sx<0||sy<0||sx>=w||sy>=h)continue;
        const src=(sy*w+sx)*4,dst=(y*hoverCanvas.width+x)*4;
        out.data[dst]=source[src];out.data[dst+1]=source[src+1];out.data[dst+2]=source[src+2];out.data[dst+3]=source[src+3];
      }
      hc.putImageData(out,0,0);
    }
  }
  return { sync, reset() { drag=null;if(move){move.local.mask=move.mask;move=null;}if(hoverFrame)cancelAnimationFrame(hoverFrame);hoverFrame=0;showHover(null);moveCursor('');sync(); } };
}
