import {slug} from './mentions.js';

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
// Session URIs are shared by mentions, formatters and the MCP tools.
export function sessionReference(href){
  try{
    const url=new URL(href);
    return url.protocol==='agentworks:'&&url.host==='sessions'&&!url.search&&!url.hash&&/^\/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(url.pathname)
      ?url.pathname.slice(1).toLowerCase():null;
  }catch{return null;}
}
// User prompts keep their literal formatting, except for session links inserted
// by the mention picker. Build DOM nodes so prompt text never becomes HTML.
export function renderUserReferences(node,text){
  node.replaceChildren();
  let start=0;
  for(const match of text.matchAll(/\[([^\]\n]+)\]\((agentworks:\/\/sessions\/[^\s)]+)\)/gi)){
    if(!sessionReference(match[2]))continue;
    node.append(document.createTextNode(text.slice(start,match.index)));
    const link=document.createElement('a');link.textContent=match[1];link.setAttribute('href',match[2]);node.append(link);
    start=match.index+match[0].length;
  }
  node.append(document.createTextNode(text.slice(start)));
}
export function setupChatReferences({getSession,getSessions,openSession,openFile,fileUrl,imageViewer}){
  return (node,base='')=>{
    const session=getSession();
    for(const link of node.querySelectorAll('a')){
      const href=link.getAttribute('href'),sessionId=sessionReference(href);
      if(sessionId){
        const target=getSessions?.().find(s=>s.id===sessionId);
        if(target)link.textContent='@'+slug(target.title);
        link.onclick=event=>{event.preventDefault();openSession?.(sessionId);};
        continue;
      }
      // Unknown Agentworks URIs are data, not an external-protocol launch.
      if(/^agentworks:/i.test(href||'')){link.removeAttribute('href');continue;}
      const reference=localReference(href,base);
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
