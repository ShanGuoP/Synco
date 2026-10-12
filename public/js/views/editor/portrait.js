// 多人人像参数面板。选择只是编辑目标，不改变其他脸已经保存的参数。
import { el, fill } from '../../core/dom.js';
import { makeProw } from '../../ui/controls.js';
import { t } from '../../core/i18n.js';

const AUTO = ['face_slim','eye_big','nose_slim','chin'];
const SHAPE = ['cheekbone','jaw_width','face_width','face_length','chin_length','eye_distance','eye_height','eye_tilt','nose_tip','nose_length','mouth_width','upper_lip','lower_lip'];
const BEAUTY = ['smooth','texture','blemish','even_tone','brighten','de_shine','sharpen'];
const RETOUCH = ['dark_circles','eye_bags','teeth','eye_light','rosy','redness','yellow'];
const MAKEUP = ['blush','lipstick','brows','gloss'];
const BODY = ['waist','shoulders','arms','legs','long_legs'];
const zero = keys => Object.fromEntries(keys.map(k => [k,0]));
export const blankPortrait = () => ({ faces:[], body:{region:[0.2,0.05,0.6,0.9],levels:[0.2,0.43,0.55,0.76],strength:zero(BODY)} });
const blankFace = idx => ({idx,auto:zero(AUTO),shape:zero(SHAPE),beauty:{...zero(BEAUTY),by_mask:false,auto_face:true},retouch:zero(RETOUCH),makeup:zero(MAKEUP),lip_color:[180,65,85],blush_color:[220,110,125],protect_features:true,protect_background:true});
export const portraitEmpty = p => !p || (p.faces || []).every(f =>
  [f.auto,f.shape,f.retouch,f.makeup].every(g => Object.values(g || {}).every(v => !v))
  && BEAUTY.every(k => k==='texture' || !f.beauty?.[k])) && Object.values(p.body?.strength || {}).every(v => !v);

export function makePortraitPanel({getOps,getAnalysis,onChange,onSelection,layer,getFrame,geometryActive}) {
  let selected=0, showBody=false;
  const controls=[], switches=[];
  const current = create => {
    const portrait=getOps().portrait;
    let f=portrait.faces.find(f => f.idx===selected);
    if (!f && create) { f=blankFace(selected); portrait.faces.push(f); }
    return f || blankFace(selected);
  };
  const ready = () => !!getAnalysis()?.faces?.find(f => f.idx===selected)?.has_landmarks;
  const changed = () => { sync(); onChange(); };
  const select=el('select.input',{'aria-label':t('portrait.select')});
  select.addEventListener('change',()=>{selected=Number(select.value);sync();onSelection?.();});
  const status=el('p.muted',{role:'status'});
  function rows(group,keys,min) {
    return keys.map(k=>{
      const ctl=makeProw({label:t(`portrait.${k}`),min,max:100,step:1,value:0,tip:t(group==='auto'||group==='shape'?'portrait.shapeTip':'portrait.localTip'),onChange:v=>{
        if (!ready()) return;
        current(true)[group][k]=v; changed();
      }});
      controls.push({ctl,group,k}); return ctl.node;
    });
  }
  function toggle(k,key,group=null) {
    const input=el('input',{type:'checkbox','aria-label':t(key)});
    input.addEventListener('change',()=>{if (!ready()) return; const f=current(true); (group?f[group]:f)[k]=input.checked; changed();});
    switches.push({input,k,group});
    return el('label.portrait-toggle',{},input,el('span',{text:t(key)}));
  }
  function color(k,key) {
    const input=el('input',{type:'color','aria-label':t(key)});
    input.addEventListener('input',()=>{if (!ready()) return;current(true)[k]=[1,3,5].map(i=>parseInt(input.value.slice(i,i+2),16));changed();});
    controls.push({color:input,k});
    return el('label.portrait-toggle',{},el('span',{text:t(key)}),input);
  }
  const section=(key,...children)=>el('details.portrait-section',{},el('summary',{text:t(key)}),el('div.portrait-section__body',{},...children));
  const faceNode=el('div.portrait-panel',{},
    select,status,el('p.muted',{text:t('portrait.faceNote')}),
    section('portrait.shapeGroup',...rows('auto',AUTO,-100),...rows('shape',SHAPE,-100),toggle('protect_background','portrait.protectBackground')),
    section('portrait.beautyGroup',...rows('beauty',BEAUTY,0),toggle('protect_features','portrait.protectFeatures'),toggle('by_mask','portrait.byMask','beauty')),
    section('portrait.retouchGroup',...rows('retouch',RETOUCH,0)),
    section('portrait.makeupGroup',...rows('makeup',MAKEUP,0),color('lip_color','portrait.lipColor'),color('blush_color','portrait.blushColor')),
    el('button.btn.btn--ghost.btn--sm',{type:'button',text:t('portrait.resetFace'),onclick:()=>{
      getOps().portrait.faces=getOps().portrait.faces.filter(f=>f.idx!==selected);changed();
    }}));
  const guide=el('div.portrait-guide',{hidden:true,'aria-hidden':'true'},
    ...['shoulder','waist','hip','knee'].map(k=>el('div.portrait-guide__line',{'data-level':k},el('span',{text:t(`portrait.level_${k}`)}))));
  layer.append(guide);
  const bodyControls=[];
  const bodyRows = (keys,group) => keys.map((k,i)=>{
    const ctl=makeProw({label:t(`portrait.${k}`),min:group==='strength'?-100:0,max:100,step:1,value:0,tip:t(group==='strength'?'portrait.bodyTip':'portrait.regionTip'),onChange:v=>{
      const b=getOps().portrait.body;
      if (group==='strength') b.strength[k]=v;
      else if (group==='region') {
        b.region[i]=Math.max(i<2?0:0.02,v/100);
        for(let a=0;a<2;a++) b.region[a]=Math.min(b.region[a],1-b.region[a+2]);
      } else { b.levels[i]=Math.max(0.05,Math.min(0.95,v/100)); b.levels.sort((a,b)=>a-b); }
      changed();
    }}); bodyControls.push({ctl,k,i,group});return ctl.node;
  });
  const show=el('input',{type:'checkbox','aria-label':t('portrait.showBody')});
  show.addEventListener('change',()=>{showBody=show.checked;syncGuide();});
  const bodyNode=el('div.portrait-panel',{},el('p.muted',{text:t('portrait.bodyNote')}),
    el('label.portrait-toggle',{},show,el('span',{text:t('portrait.showBody')})),
    section('portrait.regionGroup',...bodyRows(['region_x','region_y','region_w','region_h'],'region'),...bodyRows(['level_shoulder','level_waist','level_hip','level_knee'],'levels')),
    ...bodyRows(BODY,'strength'),
    el('button.btn.btn--ghost.btn--sm',{type:'button',text:t('portrait.resetBody'),onclick:()=>{getOps().portrait.body=blankPortrait().body;changed();}}));
  function syncGuide() {
    const frame=getFrame(); const b=getOps().portrait.body;
    guide.hidden=!showBody||geometryActive()||!frame.w;
    const [x,y,w,h]=b.region;
    Object.assign(guide.style,{left:`${x*frame.w}px`,top:`${y*frame.h}px`,width:`${w*frame.w}px`,height:`${h*frame.h}px`});
    for(let i=0;i<guide.children.length;i++) guide.children[i].style.top=`${b.levels[i]*100}%`;
  }
  function sync() {
    const analysis=getAnalysis();const faces=analysis?.faces||[];
    if (!faces.some(f=>f.idx===selected)) selected=faces[0]?.idx??0;
    fill(select,...faces.map(f=>el('option',{value:String(f.idx),text:t('portrait.faceNumber',{n:f.idx+1})})));
    select.value=String(selected);select.disabled=!faces.length;
    status.textContent=t(!faces.length?'portrait.noFaces':ready()?'portrait.selectedFace':'portrait.noMesh',{n:selected+1});
    const f=current(false);
    for(const c of controls) {
      if(c.color) { c.color.value='#'+f[c.k].map(v=>v.toString(16).padStart(2,'0')).join('');c.color.disabled=!ready(); }
      else {c.ctl.set(f[c.group]?.[c.k]??0,true);c.ctl.setDisabled(!ready()||(c.k==='texture'&&!f.beauty.smooth));}
    }
    for(const s of switches) {s.input.checked=!!(s.group?f[s.group]:f)[s.k];s.input.disabled=!ready();}
    const body=getOps().portrait.body;
    for(const c of bodyControls)c.ctl.set(c.group==='strength'?body.strength[c.k]:Math.round(body[c.group][c.i]*100),true);
    syncGuide();
  }
  return {faceNode,bodyNode,sync,syncGuide,ready,natural(){if(!ready())return;current(true).auto={face_slim:20,eye_big:12,nose_slim:10,chin:8};changed();},resetShape(){if(!ready())return;const f=current(true);f.auto=zero(AUTO);f.shape=zero(SHAPE);sync();},resetSelection(){selected=0;showBody=false;show.checked=false;sync();}};
}
