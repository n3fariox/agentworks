import {appUrl} from './hosting.js';

// Only the unpredictable class supplied by the Markdown parser can opt a fence in.
// Raw HTML language-mermaid elements and ordinary source previews stay literal.
export function enhanceMermaid(container,marker){
  if(!/^language-awmermaid-[a-f0-9]{16}$/.test(marker||''))return;
  for(const code of container.querySelectorAll('pre > code')){
    if(!code.classList.contains(marker)||code.parentElement.closest('.mermaid-block'))continue;
    code.className='language-mermaid';
    const pre=code.parentElement,doc=container.ownerDocument,block=doc.createElement('figure');
    block.className='mermaid-block';
    const button=doc.createElement('button');button.type='button';button.className='mermaid-toggle';
    const frame=doc.createElement('iframe');frame.className='mermaid-diagram';frame.title='Mermaid diagram';
    frame.setAttribute('sandbox','allow-scripts');frame.setAttribute('referrerpolicy','no-referrer');
    const status=doc.createElement('p');status.className='mermaid-status';status.setAttribute('role','status');status.textContent='Rendering diagram…';
    pre.id='mermaid-source-'+crypto.getRandomValues(new Uint32Array(2)).join('-');pre.tabIndex=0;pre.setAttribute('aria-label','Mermaid source');
    button.setAttribute('aria-controls',pre.id);
    let source=false,failed=false,done=false;
    function show(){pre.hidden=!source;frame.hidden=source||failed;button.textContent=source?'Show diagram':'Show source';button.setAttribute('aria-expanded',String(source));}
    button.onclick=()=>{source=!source;show();};
    pre.replaceWith(block);block.append(button,frame,pre,status);show();
    function finish(error,height){
      if(done)return;done=true;clearTimeout(timer);window.removeEventListener('message',receive);
      if(error){failed=true;source=true;button.hidden=true;frame.remove();block.classList.add('mermaid-error');status.textContent='Unable to render Mermaid diagram: '+error;}
      else{status.hidden=true;frame.style.height=Math.min(1600,Math.max(80,height||200))+'px';}
      show();
    }
    function receive(event){
      if(event.source!==frame.contentWindow||event.data?.type!=='agentworks-mermaid-result')return;
      finish(typeof event.data.error==='string'?event.data.error.slice(0,500):'',Number(event.data.height));
    }
    window.addEventListener('message',receive);
    const timer=setTimeout(()=>finish('Renderer timed out or could not load. The original source is shown.'),15000);
    frame.onload=()=>{if(!done)frame.contentWindow.postMessage({type:'agentworks-mermaid-render',source:code.textContent},'*');};
    frame.src=appUrl('/mermaid-frame.html');
  }
}
