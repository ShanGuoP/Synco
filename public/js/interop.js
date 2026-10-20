// 外部交接的待编辑入口；推送只在空闲页面自动打开，不抢占当前编辑会话。
import {el,fill} from './core/dom.js';
import {api} from './core/api.js';
import {t,dx} from './core/i18n.js';
import {go} from './core/router.js';
import {interopEditor,isEditorOpen,beforeLeaveEditor} from './views/editor/index.js';
import {isCanvasOpen} from './views/canvas/index.js';
import {confirm} from './ui/modal.js';
import {toastErr,toastOk} from './ui/toast.js';

export function startInterop() {
  let rows=[],busy=false,stopped=false,navigating=null,attempts=0,signature='',timer;
  const key=task=>`${task.client_id}/${task.task_id}`;
  const title=el('span.interop-bar__label');
  const picker=el('select.input',{'aria-label':t('interop.pending')});
  const open=el('button.btn.btn--ghost.btn--sm',{type:'button',text:t('interop.open'),onclick:()=>openTask(rows.find(r=>key(r)===picker.value))});
  const finish=el('button.btn.btn--primary.btn--sm',{type:'button',text:t('interop.complete'),onclick:complete});
  const discard=el('button.btn.btn--ghost.btn--sm',{type:'button',text:t('interop.discard'),onclick:cancel});
  const bar=el('div.interop-bar',{hidden:true},title,picker,open,finish,discard);document.body.append(bar);
  const current=()=>{const view=interopEditor();return rows.find(r=>r.image_id===view?.imageId);};
  async function openTask(task) {
    if(!task||busy||navigating)return;
    navigating=task.task_id;go(`/p/${task.project_id}/e/${task.image_id}`);
    // 只有编辑器真正加载了目标图才消费打开意图；失败或刷新后仍可重试。
    for(let i=0;i<50&&!stopped;i++) {
      await new Promise(resolve=>setTimeout(resolve,100));
      if(interopEditor()?.imageId===task.image_id&&interopEditor()?.ready){try{await api.interopOpened(task);}catch(e){toastErr(t('interop.failed'),e.message);}break;}
    }
    navigating=null;
  }
  async function complete() {
    const task=current(),view=interopEditor();if(!task||task.state!=='editing'||busy||!view.ready)return;
    const editor=document.getElementById('editor'),wasInert=editor.inert;editor.inert=true;
    busy=true;paint();
    try {
      await beforeLeaveEditor();
      if(interopEditor()?.imageId!==view.imageId)return;
      await api.interopComplete(task,view.resultId);
      toastOk(t('interop.rendering'));
    }catch(e){toastErr(t('interop.failed'),e.message);}finally{editor.inert=wasInert;busy=false;await poll();}
  }
  async function cancel() {
    const task=current();if(!task||busy||!await confirm({title:t('interop.discard'),body:t('interop.discardHint'),ok:t('interop.discard')}))return;
    busy=true;paint();try{await api.interopCancel(task);}catch(e){toastErr(t('interop.failed'),e.message);}finally{busy=false;await poll();}
  }
  function paint() {
    const task=current(),pending=rows.filter(r=>r.state==='editing');
    bar.hidden=!task&&!pending.length;
    title.textContent=task?t(`interop.${task.state}`,{name:task.project_name}):t('interop.pendingCount',{n:pending.length});
    const nextSignature=JSON.stringify(pending.map(r=>[r.task_id,r.client_id,r.project_name,r.image_name]));
    if(signature!==nextSignature){const selected=picker.value;fill(picker,...pending.map(r=>el('option',{value:key(r),text:`${r.project_name} · ${r.image_name}`})));if(pending.some(r=>key(r)===selected))picker.value=selected;signature=nextSignature;}
    picker.hidden=!pending.length;open.hidden=!pending.length;open.disabled=busy||!!navigating;
    finish.hidden=!task||task.state!=='editing';finish.disabled=busy||!interopEditor()?.ready;
    finish.textContent=t(interopEditor()?.resultId?'interop.completeResult':'interop.complete');
    discard.hidden=!task||!['editing','ready'].includes(task.state);discard.disabled=busy;
    if(task?.last_error)title.textContent+=` · ${dx(task.last_error.message_key)||task.last_error.message}`;
  }
  let polling=false;
  async function poll() {
    if(stopped||polling)return;polling=true;
    try {
      const data=await api.interopInbox();if(stopped)return;rows=data.tasks;attempts=0;paint();
      const auto=rows.find(r=>r.state==='editing'&&r.open_in_editor&&!r.opened);
      const typing=['INPUT','TEXTAREA','SELECT'].includes(document.activeElement?.tagName);
      if(auto&&!navigating&&!busy&&!isEditorOpen()&&!isCanvasOpen()&&!typing&&!document.hidden&&!document.querySelector('[role="dialog"]'))void openTask(auto);
    }catch{attempts++;}finally{polling=false;}
  }
  async function tick(){await poll();if(!stopped)timer=setTimeout(tick,Math.min(30000,1500*2**Math.min(attempts,4)));}
  document.addEventListener('pointerover',paint);
  window.addEventListener('pagehide',()=>{stopped=true;clearTimeout(timer);},{once:true});
  void tick();
}
