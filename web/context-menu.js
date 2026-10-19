// One delegated menu supports rows that are replaced while sessions stream.
export function setupContextMenu({resolve,onError}){
  const menu=document.createElement('div');menu.className='context-menu';menu.role='menu';menu.hidden=true;document.body.append(menu);
  let origin=null,hold=null,suppressed=null;
  const cancelHold=()=>{if(hold)clearTimeout(hold.timer);hold=null;};
  function close(restore=false){menu.hidden=true;menu.replaceChildren();if(restore&&origin?.isConnected)origin.focus();origin=null;}
  function open(target,x,y){
    const context=resolve(target);if(!context)return false;
    close();origin=context.origin;menu.setAttribute('aria-label',context.label);
    for(const action of context.actions){
      const button=document.createElement('button');button.type='button';button.role='menuitem';button.textContent=action.label;button.classList.toggle('danger',!!action.danger);
      button.onclick=async()=>{close(true);try{await action.run();}catch(error){onError(error.message);}};menu.append(button);
    }
    menu.hidden=false;
    menu.style.left=Math.max(8,Math.min(x,innerWidth-menu.offsetWidth-8))+'px';
    menu.style.top=Math.max(8,Math.min(y,innerHeight-menu.offsetHeight-8))+'px';
    menu.querySelector('button')?.focus({preventScroll:true});return true;
  }
  document.addEventListener('contextmenu',event=>{
    if(suppressed&&suppressed.until>Date.now()&&suppressed.target.contains(event.target)){event.preventDefault();return;}
    cancelHold();if(open(event.target,event.clientX,event.clientY))event.preventDefault();else close();
  });
  document.addEventListener('pointerdown',event=>{
    cancelHold();suppressed=null;if(!menu.hidden&&!menu.contains(event.target))close();
    if(event.pointerType==='mouse'||!event.isPrimary||event.button!==0||!resolve(event.target))return;
    const {target,clientX:x,clientY:y,pointerId}=event;
    hold={x,y,pointerId,timer:setTimeout(()=>{hold=null;if(open(target,x,y))suppressed={target:origin,until:Date.now()+1500};},550)};
  });
  document.addEventListener('pointermove',event=>{if(hold&&event.pointerId===hold.pointerId&&Math.hypot(event.clientX-hold.x,event.clientY-hold.y)>10)cancelHold();});
  for(const type of ['pointerup','pointercancel'])document.addEventListener(type,cancelHold);
  document.addEventListener('click',event=>{
    if(suppressed&&suppressed.until>Date.now()&&suppressed.target.contains(event.target)){event.preventDefault();event.stopImmediatePropagation();suppressed=null;}
  },true);
  document.addEventListener('scroll',()=>{cancelHold();close();},true);
  window.addEventListener('resize',()=>{cancelHold();close();});
  document.addEventListener('keydown',event=>{
    if(!menu.hidden){
      const buttons=[...menu.querySelectorAll('button')],index=buttons.indexOf(document.activeElement);
      if(event.key==='Escape'||event.key==='Tab'){close(event.key==='Escape');if(event.key==='Escape')event.preventDefault();}
      else if(['ArrowDown','ArrowUp','Home','End'].includes(event.key)){
        event.preventDefault();buttons[event.key==='Home'?0:event.key==='End'?buttons.length-1:(index+(event.key==='ArrowDown'?1:-1)+buttons.length)%buttons.length]?.focus();
      }
    }else if(event.key==='ContextMenu'||(event.shiftKey&&event.key==='F10')){
      const rect=event.target.getBoundingClientRect();if(open(event.target,rect.left+16,rect.top+16))event.preventDefault();
    }
  });
  return {close};
}
