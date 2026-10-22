import {localState as localStorage} from './hosting.js';
// Desktop sessions panel: pinned (a grid column, the default) or unpinned (no
// column; the Sessions nav button opens it as an overlay that closes on an
// outside click, Escape, or picking a session). Hovering the nav rail unfolds
// it too, and it folds away again when the pointer leaves the rail and panel.
// Phone layouts keep their own drawer, so CSS ignores both states below 801px.
export function setupSidebarPin({sidebar,button,trigger,key='agentworks-sidebar-pinned'}){
  const desktop=matchMedia('(min-width:801px)');
  const hoverCapable=matchMedia('(hover:hover)');
  const body=document.body;
  const nav=trigger.closest('nav')||trigger;
  const OPEN_DELAY=140,CLOSE_DELAY=180;
  let hoverOpened=false,openTimer=null,closeTimer=null;
  const clearTimers=()=>{clearTimeout(openTimer);clearTimeout(closeTimer);openTimer=closeTimer=null;};
  const isOverlay=()=>desktop.matches&&body.classList.contains('sidebar-unpinned');
  const setOpen=open=>{if(!open)hoverOpened=false;body.classList.toggle('sidebar-open',open);trigger.setAttribute('aria-expanded',String(open));};
  const setPinned=pinned=>{
    clearTimers();hoverOpened=false;
    body.classList.toggle('sidebar-unpinned',!pinned);
    button.setAttribute('aria-pressed',String(pinned));button.textContent=pinned?'⇤':'⇥';
    button.title=pinned?'Hide sessions panel':'Pin sessions panel';
    if(pinned)localStorage.removeItem(key);else localStorage.setItem(key,'false');
    setOpen(false);
    if(pinned)trigger.removeAttribute('aria-expanded');
  };
  const canHover=()=>hoverCapable.matches&&isOverlay()&&body.dataset.workspaceView!=='files'&&body.dataset.workspaceView!=='automations'&&!document.querySelector('dialog[open]');
  const openForHover=()=>{
    if(!canHover()||body.classList.contains('sidebar-open'))return;
    clearTimeout(openTimer);
    openTimer=setTimeout(()=>{
      openTimer=null;
      if(!canHover()||body.classList.contains('sidebar-open'))return;
      hoverOpened=true;setOpen(true);
    },OPEN_DELAY);
  };
  const closeAfterHover=()=>{
    clearTimeout(openTimer);openTimer=null;
    if(!hoverOpened||!body.classList.contains('sidebar-open'))return;
    clearTimeout(closeTimer);
    closeTimer=setTimeout(()=>{
      closeTimer=null;
      if(!hoverOpened||!isOverlay())return;
      if(nav.matches(':hover')||sidebar.matches(':hover'))return;
      if(sidebar.contains(document.activeElement)&&document.activeElement.matches?.(':focus-visible'))return;
      setOpen(false);
    },CLOSE_DELAY);
  };
  const cancelClose=()=>{clearTimeout(closeTimer);closeTimer=null;};
  nav.addEventListener('pointerenter',()=>{if(!hoverCapable.matches||!isOverlay())return;cancelClose();openForHover();});
  nav.addEventListener('pointerleave',closeAfterHover);
  sidebar.addEventListener('pointerenter',cancelClose);
  sidebar.addEventListener('pointerleave',closeAfterHover);
  button.addEventListener('click',()=>setPinned(body.classList.contains('sidebar-unpinned')));
  trigger.addEventListener('click',()=>{
    if(!isOverlay())return;
    if(!body.classList.contains('sidebar-open')){clearTimers();hoverOpened=false;setOpen(true);}
    else if(hoverOpened){cancelClose();hoverOpened=false;} // A click keeps a hover-opened panel open.
    else setOpen(false);
  });
  document.addEventListener('pointerdown',event=>{if(body.classList.contains('sidebar-open')&&!sidebar.contains(event.target)&&!trigger.contains(event.target)&&!event.target.closest?.('dialog'))setOpen(false);});
  document.addEventListener('keydown',event=>{if(event.key==='Escape'&&body.classList.contains('sidebar-open')&&!document.querySelector('dialog[open]')){setOpen(false);trigger.focus();}});
  setPinned(localStorage.getItem(key)!=='false');
  return {close:()=>{clearTimers();setOpen(false);}};
}
