import {localState as localStorage} from './hosting.js';
// Desktop sessions panel: pinned (a grid column, the default) or unpinned (no
// column; the Sessions nav button opens it as an overlay that closes on an
// outside click, Escape, or picking a session). Phone layouts keep their own
// drawer, so CSS ignores both states below 801px.
export function setupSidebarPin({sidebar,button,trigger,key='agentworks-sidebar-pinned'}){
  const desktop=matchMedia('(min-width:801px)');
  const body=document.body;
  const setOpen=open=>{body.classList.toggle('sidebar-open',open);trigger.setAttribute('aria-expanded',String(open));};
  const setPinned=pinned=>{
    body.classList.toggle('sidebar-unpinned',!pinned);
    button.setAttribute('aria-pressed',String(pinned));button.textContent=pinned?'⇤':'⇥';
    button.title=pinned?'Hide sessions panel':'Pin sessions panel';
    if(pinned)localStorage.removeItem(key);else localStorage.setItem(key,'false');
    if(pinned)trigger.removeAttribute('aria-expanded');else setOpen(false);
  };
  const isOverlay=()=>desktop.matches&&body.classList.contains('sidebar-unpinned');
  button.addEventListener('click',()=>setPinned(body.classList.contains('sidebar-unpinned')));
  trigger.addEventListener('click',()=>{if(isOverlay())setOpen(!body.classList.contains('sidebar-open'));});
  document.addEventListener('pointerdown',event=>{if(body.classList.contains('sidebar-open')&&!sidebar.contains(event.target)&&!trigger.contains(event.target)&&!event.target.closest?.('dialog'))setOpen(false);});
  document.addEventListener('keydown',event=>{if(event.key==='Escape'&&body.classList.contains('sidebar-open')&&!document.querySelector('dialog[open]')){setOpen(false);trigger.focus();}});
  setPinned(localStorage.getItem(key)!=='false');
  return {close:()=>{if(body.classList.contains('sidebar-open'))setOpen(false);}};
}
