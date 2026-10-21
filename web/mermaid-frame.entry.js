// Built into one checked-in script: no dynamic imports, CDN, fonts or runtime fetches.
import mermaid from 'mermaid';
import DOMPurify from 'dompurify';
let started=false;
window.addEventListener('message',async event=>{
  if(event.source!==parent||event.data?.type!=='agentworks-mermaid-render'||started)return;
  started=true;
  const reply=data=>parent.postMessage({type:'agentworks-mermaid-result',...data},'*');
  try{
    const source=event.data.source;
    if(typeof source!=='string'||source.length>24000)throw new Error('Diagram exceeds the 24,000 character limit.');
    // Configuration can inject CSS or enable external resources. Keep it application-owned.
    if(/%%\s*\{|^\s*---/.test(source))throw new Error('Diagram configuration directives and front matter are unsupported.');
    mermaid.initialize({startOnLoad:false,securityLevel:'strict',theme:'dark',htmlLabels:false,
      flowchart:{htmlLabels:false},maxTextSize:24000,maxEdges:300,suppressErrorRendering:true});
    const {svg}=await mermaid.render('diagram',source);
    const clean=DOMPurify.sanitize(svg,{USE_PROFILES:{svg:true,svgFilters:true},
      FORBID_TAGS:['foreignObject','a','image','script','iframe','animate','animateMotion','animateTransform','set'],
      FORBID_ATTR:['target','src','tabindex']});
    const parsed=new DOMParser().parseFromString(clean,'image/svg+xml'),root=parsed.documentElement;
    if(root.localName!=='svg'||parsed.querySelector('parsererror'))throw new Error('Renderer returned invalid SVG.');
    for(const node of [root,...root.querySelectorAll('*')]){
      for(const attr of [...node.attributes]){
        // References are local fragments only; all navigation and resource URLs are removed.
        if((/href$/i.test(attr.name)&&!/^#[\w:.-]+$/.test(attr.value))||/^on/i.test(attr.name)||
          (/url\s*\(/i.test(attr.value)&&!/^(?:[^u]|u(?!rl\s*\())*url\(#[\w:.-]+\)[^()]*$/i.test(attr.value)))node.removeAttributeNode(attr);
      }
    }
    root.setAttribute('role','img');root.setAttribute('aria-label','Mermaid diagram');
    document.body.replaceChildren(document.importNode(root,true));
    // Diagram CSS is contained by the opaque sandbox; CSP also blocks CSS resource loads.
    reply({height:Math.ceil(document.body.getBoundingClientRect().height)});
  }catch(error){
    document.body.replaceChildren();reply({error:String(error?.message||error).slice(0,500)});
  }
});
