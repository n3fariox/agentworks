export function setupImageViewer(messages){
  const dialog=document.createElement('dialog');dialog.className='image-viewer';dialog.setAttribute('aria-label','Image viewer');
  dialog.innerHTML='<div class="image-toolbar"><button type="button" aria-label="Zoom out">−</button><output aria-live="polite"></output><button type="button" aria-label="Zoom in">+</button><button type="button" aria-label="Actual size">100%</button><button type="button" aria-label="Fit image">Fit</button><button type="button" aria-label="Close image viewer">×</button></div><p class="sr-only" id="image-viewer-help">Use plus and minus to zoom, 0 to fit, arrow keys to pan, or drag. Scroll or pinch to zoom. Escape closes.</p><div class="image-viewport" tabindex="0" aria-label="Enlarged image" aria-describedby="image-viewer-help"><img draggable="false"></div>';
  document.body.append(dialog);
  const viewport=dialog.querySelector('.image-viewport'),image=viewport.querySelector('img'),output=dialog.querySelector('output'),buttons=dialog.querySelectorAll('button');
  let scale=1,x=0,y=0,opener,chatTop=0;
  const pointers=new Map();
  const draw=()=>{image.style.width=image.naturalWidth*scale+'px';image.style.height=image.naturalHeight*scale+'px';image.style.transform=`translate(${x}px, ${y}px)`;output.textContent=Math.round(scale*100)+'%';};
  function zoom(next,cx=viewport.clientWidth/2,cy=viewport.clientHeight/2){
    next=Math.min(16,Math.max(.05,next));const ratio=next/scale;
    x=cx-(cx-x)*ratio;y=cy-(cy-y)*ratio;scale=next;draw();
  }
  function fit(){if(!image.naturalWidth)return;scale=Math.min(1,viewport.clientWidth/image.naturalWidth,viewport.clientHeight/image.naturalHeight);x=(viewport.clientWidth-image.naturalWidth*scale)/2;y=(viewport.clientHeight-image.naturalHeight*scale)/2;draw();}
  buttons[0].onclick=()=>zoom(scale/1.25);buttons[1].onclick=()=>zoom(scale*1.25);buttons[2].onclick=()=>zoom(1);buttons[3].onclick=fit;buttons[4].onclick=()=>dialog.close();
  image.onload=fit;
  dialog.addEventListener('close',()=>{pointers.clear();opener?.focus({preventScroll:true});messages.scrollTo({top:chatTop,behavior:'instant'});image.removeAttribute('src');});
  dialog.addEventListener('keydown',event=>{
    if(event.key==='Tab'){
      if(event.shiftKey&&document.activeElement===buttons[0]){event.preventDefault();viewport.focus();}
      else if(!event.shiftKey&&document.activeElement===viewport){event.preventDefault();buttons[0].focus();}
      return;
    }
    if(['+','=','-','0','ArrowLeft','ArrowRight','ArrowUp','ArrowDown'].includes(event.key)){
      event.preventDefault();
      if(event.key==='0')fit();else if(event.key==='-')zoom(scale/1.25);else if(['+','='].includes(event.key))zoom(scale*1.25);
      else{x+=event.key==='ArrowLeft'?40:event.key==='ArrowRight'?-40:0;y+=event.key==='ArrowUp'?40:event.key==='ArrowDown'?-40:0;draw();}
    }
  });
  viewport.addEventListener('wheel',event=>{event.preventDefault();const rect=viewport.getBoundingClientRect();zoom(scale*Math.exp(-event.deltaY*.002),event.clientX-rect.left,event.clientY-rect.top);},{passive:false});
  viewport.addEventListener('dblclick',()=>scale===1?fit():zoom(1));
  viewport.addEventListener('pointerdown',event=>{pointers.set(event.pointerId,{x:event.clientX,y:event.clientY});viewport.setPointerCapture(event.pointerId);});
  viewport.addEventListener('pointermove',event=>{
    const previous=pointers.get(event.pointerId);if(!previous)return;
    const other=[...pointers.entries()].find(([id])=>id!==event.pointerId)?.[1];
    if(other){
      const before=Math.hypot(previous.x-other.x,previous.y-other.y),after=Math.hypot(event.clientX-other.x,event.clientY-other.y),rect=viewport.getBoundingClientRect();
      if(before)zoom(scale*after/before,(event.clientX+other.x)/2-rect.left,(event.clientY+other.y)/2-rect.top);
    }else{x+=event.clientX-previous.x;y+=event.clientY-previous.y;draw();}
    pointers.set(event.pointerId,{x:event.clientX,y:event.clientY});
  });
  for(const name of ['pointerup','pointercancel','lostpointercapture'])viewport.addEventListener(name,event=>pointers.delete(event.pointerId));
  new ResizeObserver(()=>{if(dialog.open)fit();}).observe(viewport);
  return {decorate(img){
    img.tabIndex=0;img.setAttribute('role','button');img.setAttribute('aria-label',`Enlarge image: ${img.alt}`);img.classList.add('zoomable-image');
    const open=event=>{event.preventDefault();event.stopPropagation();opener=img;chatTop=messages.scrollTop;dialog.showModal();image.alt=img.alt;image.src=img.currentSrc||img.src;if(image.complete)fit();buttons[4].focus();};
    img.onclick=open;img.onkeydown=event=>{if(event.key==='Enter'||event.key===' ')open(event);};
  }};
}
