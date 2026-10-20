import {localState as localStorage} from './hosting.js';
// Drag or arrow-key a panel edge to resize it on desktop. The width is a CSS
// variable on `target` and persists in localStorage under `key`; double-click
// resets it. `edge` is the side of `panel` the handle sits on. Phone layouts
// hide the handles.
export const clamp=(w,min,max)=>Math.round(Math.min(max,Math.max(min,w)));

export function setupResize({handle,target,panel,variable,key,min,max,initial,edge='right'}){
  const apply=w=>{
    if(w==null){target.style.removeProperty(variable);localStorage.removeItem(key);}
    else{w=clamp(w,min,max);target.style.setProperty(variable,w+'px');localStorage.setItem(key,String(w));}
    handle.setAttribute('aria-valuenow',String(w??initial));
  };
  handle.setAttribute('aria-valuemin',String(min));handle.setAttribute('aria-valuemax',String(max));
  const saved=Number(localStorage.getItem(key));apply(saved?saved:null);
  // Width from the pointer: the far edge of the panel stays put.
  const widthAt=x=>{const r=panel.getBoundingClientRect();return edge==='right'?x-r.left:r.right-x;};
  handle.addEventListener('pointerdown',event=>{
    if(event.button!==0)return;
    event.preventDefault();handle.setPointerCapture(event.pointerId);document.body.classList.add('resizing');
    const move=e=>apply(widthAt(e.clientX));
    const up=()=>{handle.removeEventListener('pointermove',move);handle.removeEventListener('pointerup',up);handle.removeEventListener('pointercancel',up);document.body.classList.remove('resizing');};
    handle.addEventListener('pointermove',move);handle.addEventListener('pointerup',up);handle.addEventListener('pointercancel',up);
  });
  handle.addEventListener('dblclick',()=>apply(null));
  handle.addEventListener('keydown',event=>{
    const step=event.shiftKey?40:10,now=panel.getBoundingClientRect().width,grow=edge==='right'?'ArrowRight':'ArrowLeft';
    if(event.key===grow)apply(now+step);else if(event.key==='ArrowLeft'||event.key==='ArrowRight')apply(now-step);
    else if(event.key==='Home')apply(min);else if(event.key==='End')apply(max);else return;
    event.preventDefault();
  });
}

// Native vertical resizing sets a preferred minimum. Automatic sizing may grow
// above it for content, but never collapses the user's chosen writing space.
export function setupPromptResize(area){
  let preferred=0,expected='',dragging=false,dragHeight=0;
  const size=()=>{
    if(dragging||!area.getClientRects().length)return;
    if(area.style.height&&area.style.height!==expected)preferred=area.getBoundingClientRect().height;
    const css=getComputedStyle(area),min=parseFloat(css.minHeight)||48,max=parseFloat(css.maxHeight)||320;
    area.style.height='0px';
    const border=area.offsetHeight-area.clientHeight;
    const height=Math.min(max,Math.max(min,preferred,area.scrollHeight+border));
    expected=height+'px';area.style.height=expected;
  };
  const observer=new ResizeObserver(()=>{
    // Native resize changes inline height; layout/width changes do not.
    if(area.style.height&&area.style.height!==expected){preferred=area.getBoundingClientRect().height;expected=area.style.height;}
    size();
  });observer.observe(area);
  area.addEventListener('pointerdown',event=>{
    const r=area.getBoundingClientRect();
    if(event.button===0&&event.clientX>=r.right-20&&event.clientY>=r.bottom-20){dragging=true;dragHeight=r.height;}
  });
  const finish=()=>{if(!dragging)return;dragging=false;const height=area.getBoundingClientRect().height;if(Math.abs(height-dragHeight)>.5)preferred=height;expected=area.style.height;size();};
  window.addEventListener('pointerup',finish);window.addEventListener('pointercancel',finish);
  area.addEventListener('input',size);window.addEventListener('resize',size);size();
  return {size};
}
