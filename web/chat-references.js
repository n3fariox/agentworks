// Interpret only local URLs. External URLs retain their path, query and fragment.
export function localReference(href,base=''){
  if(!href||href.startsWith('#'))return null;
  try{
    let path;
    if(/^file:/i.test(href)){
      const url=new URL(href);
      if(!['','localhost'].includes(url.hostname))return null;
      path=decodeURIComponent(url.pathname);
    }else{
      if(/^[a-z][a-z0-9+.-]*:/i.test(href.replace(/:\d+(?::\d+)?$/,''))||href.startsWith('//'))return null;
      path=decodeURIComponent(href.split('#')[0]);
    }
    return path.startsWith('/')?path:base+path.replace(/^\.\//,'');
  }catch{return null;}
}
export function setupChatReferences({getSession,openFile,fileUrl,imageViewer}){
  return (node,base='')=>{
    const session=getSession();
    for(const link of node.querySelectorAll('a')){
      const href=link.getAttribute('href'),reference=localReference(href,base);
      if(reference!==null){
        const suffix=/:(\d+)(?::\d+)?$/.exec(reference);
        const line=suffix&&Number(suffix[1])>0?Number(suffix[1]):null;
        const path=line?reference.slice(0,suffix.index):reference;
        link.href='#';link.onclick=event=>{event.preventDefault();if(session?.id===getSession()?.id)openFile(session,path,line);};
      }else if(/^file:/i.test(href||''))link.removeAttribute('href');
      else{link.target='_blank';link.rel='noopener noreferrer';}
    }
    for(const img of node.querySelectorAll('img')){
      const path=localReference(img.getAttribute('src'),base);
      if(path!==null)img.src=fileUrl(path);
      img.loading='lazy';img.alt||='Agent image';imageViewer.decorate(img);
    }
  };
}
