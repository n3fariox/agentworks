// Desktop session browser width: drag or arrow-key the sidebar's right edge.
// The width is a CSS variable on the layout and persists in localStorage;
// phone layouts use an overlay sidebar and hide the handle.
export const MIN=200,MAX=560,DEFAULT=228;
const KEY='agentworks-sidebar-width';
export const clamp=w=>Math.round(Math.min(MAX,Math.max(MIN,w)));

export function setupSidebarResize({handle,layout,sidebar}){
  const apply=w=>{
    if(w==null){layout.style.removeProperty('--sidebar-width');localStorage.removeItem(KEY);}
    else{w=clamp(w);layout.style.setProperty('--sidebar-width',w+'px');localStorage.setItem(KEY,String(w));}
    handle.setAttribute('aria-valuenow',String(w??DEFAULT));
  };
  handle.setAttribute('aria-valuemin',String(MIN));handle.setAttribute('aria-valuemax',String(MAX));
  const saved=Number(localStorage.getItem(KEY));apply(saved?saved:null);
  handle.addEventListener('pointerdown',event=>{
    if(event.button!==0)return;
    event.preventDefault();handle.setPointerCapture(event.pointerId);
    const left=sidebar.getBoundingClientRect().left;document.body.classList.add('sidebar-resizing');
    const move=e=>apply(e.clientX-left);
    const up=()=>{handle.removeEventListener('pointermove',move);handle.removeEventListener('pointerup',up);handle.removeEventListener('pointercancel',up);document.body.classList.remove('sidebar-resizing');};
    handle.addEventListener('pointermove',move);handle.addEventListener('pointerup',up);handle.addEventListener('pointercancel',up);
  });
  handle.addEventListener('dblclick',()=>apply(null));
  handle.addEventListener('keydown',event=>{
    const step=event.shiftKey?40:10,now=sidebar.getBoundingClientRect().width;
    if(event.key==='ArrowLeft')apply(now-step);else if(event.key==='ArrowRight')apply(now+step);
    else if(event.key==='Home')apply(MIN);else if(event.key==='End')apply(MAX);else return;
    event.preventDefault();
  });
}
