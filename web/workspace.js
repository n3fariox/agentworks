import {enhanceMermaid} from './mermaid.js';
import {appUrl, localState as localStorage, sessionState as sessionStorage} from './hosting.js';
export function createWorkspace(hooks){
  const $=id=>document.getElementById(id),el=(tag,cls,text)=>{const node=document.createElement(tag);if(cls)node.className=cls;if(text!==undefined)node.textContent=text;return node;};
  const read=(storage,key,fallback)=>{try{return JSON.parse(storage.getItem(key))??fallback;}catch{return fallback;}};
  const initial=read(localStorage,'agentworks-workspace',{});
  const requestedView=new URLSearchParams(location.search).get('view');
  if(['sessions','files','automations'].includes(requestedView))initial.view=requestedView;
  const state={view:'sessions',projectId:initial.projectId||null,locationKey:initial.locationKey||'',path:'',hidden:false,file:null,sideFile:null,automationId:initial.automationId||null};
  let catalog={projects:[],locations:[],hosts:[]},sessions=[],definitions=[],runs=[],contextGeneration=0,filesGeneration=0,automationGeneration=0,editorGeneration=0,editing=null,runStarting=false,pendingFile=null;
  const pendingRuns=read(sessionStorage,'agentworks-pending-runs',{});
  const api=hooks.api;
  const save=()=>localStorage.setItem('agentworks-workspace',JSON.stringify({view:state.view,projectId:state.projectId,locationKey:state.locationKey,path:state.path,filePath:state.file?.path||null,automationId:state.automationId}));
  function option(select,value,label){const node=el('option','',label);node.value=value;select.append(node);}
  function options(select,items,value){const signature=JSON.stringify(items);if(select.dataset.items!==signature){select.replaceChildren();for(const [id,label]of items)option(select,id,label);select.dataset.items=signature;}select.value=value;}
  function projectOptions(select,value){options(select,[['','Ungrouped'],...catalog.projects.filter(p=>!p.archived||p.id===value).map(p=>[p.id,p.name])],value||'');}
  const shortPath=path=>path.replaceAll('\\','/').split('/').filter(Boolean).slice(-2).join('/')||'/';
  const currentLocation=()=>catalog.locations.find(l=>l.id===state.locationKey);
  function directory(){return currentLocation()?.path||(state.locationKey.startsWith('root:')?state.locationKey.slice(5):hooks.getConfig()?.roots[0])||'';}
  function contextControls(){
    if(state.projectId&&!catalog.projects.some(p=>p.id===state.projectId))state.projectId=null;
    projectOptions($('workspace-project'),state.projectId);
    const project=catalog.projects.find(p=>p.id===state.projectId);
    const normalized=path=>path.replaceAll('\\','/').replace(/\/$/,'');
    const allowed=l=>(hooks.getConfig()?.roots||[]).some(root=>normalized(l.path)===normalized(root)||normalized(l.path).startsWith(normalized(root)+'/'));
    const locations=catalog.locations.filter(l=>l.hostId==='local'&&allowed(l)&&(!project||project.locationIds?.includes(l.id)||sessions.some(s=>s.projectId===project.id&&s.locationId===l.id)));
    const items=locations.map(l=>[l.id,'Local · '+shortPath(l.path)]);
    for(const root of hooks.getConfig()?.roots||[])if(!locations.some(l=>l.path===root))items.push(['root:'+root,'Local · '+shortPath(root)]);
    if(!items.some(([id])=>id===state.locationKey))state.locationKey=items[0]?.[0]||'';
    options($('workspace-location'),items,state.locationKey);$('workspace-location').title=directory();
  }
  function resetFiles(){filesGeneration++;state.path='';state.file=null;$('workspace-file-detail').replaceChildren(el('div','workspace-empty','Choose a file to preview.'));}
  async function ensureLocation(){
    const existing=currentLocation();if(existing)return existing;
    const cwd=directory();if(!cwd)throw new Error('Add an allowed root in Settings first.');
    const generation=contextGeneration,projectId=state.projectId;
    const location=await api('/api/locations',{method:'POST',body:JSON.stringify({cwd,projectId})});
    if(generation!==contextGeneration)throw new Error('Directory selection changed.');
    catalog.locations=catalog.locations.filter(l=>l.id!==location.id).concat(location);
    const project=catalog.projects.find(p=>p.id===projectId);if(project&&!project.locationIds?.includes(location.id))(project.locationIds??=[]).push(location.id);
    state.locationKey=location.id;contextControls();save();return location;
  }
  function showView(view){
    state.view=['sessions','files','automations','host-terminal'].includes(view)?view:'sessions';
    document.body.dataset.workspaceView=state.view;document.body.classList.remove('menu-open');
    $('host-terminal-view').hidden=state.view!=='host-terminal';$('workspace-session-area').hidden=state.view!=='sessions';$('workspace-files').hidden=state.view!=='files';$('workspace-automations').hidden=state.view!=='automations';
    for(const name of ['sessions','files','automations'])$('nav-'+name).setAttribute('aria-pressed',String(state.view===name||(name==='sessions'&&state.view==='host-terminal')));
    if(state.view==='files')loadFiles();if(state.view==='automations')loadAutomations();hooks.viewChanged?.(state.view);save();
  }
  // Directory listing shared by the Files view and the side panel's Files tab.
  function renderListing({crumbs,list,result,go,open,selected}){
    crumbs.replaceChildren();
    const root=el('button','text-button','Root');root.onclick=()=>go('');crumbs.append(root);
    const segments=result.path.split('/').filter(Boolean);for(let i=0;i<segments.length;i++){const button=el('button','text-button',segments[i]);button.onclick=()=>go(segments.slice(0,i+1).join('/'));crumbs.append(el('span','','/'),button);}
    list.replaceChildren();
    if(result.path){const parent=el('button','file-entry','↰ Parent directory');parent.onclick=()=>go(result.path.split('/').slice(0,-1).join('/'));list.append(parent);}
    for(const entry of result.entries){
      const button=el('button','file-entry'+(selected===entry.path?' selected':''));button.append(el('span','file-kind',entry.kind==='directory'?'▸':entry.kind==='blocked'?'↗':'▤'),el('span','file-name',entry.name));
      button.dataset.path=entry.path;button.title=entry.kind==='blocked'?'This entry cannot be opened inside this project directory':entry.name;button.disabled=entry.kind==='blocked';
      button.onclick=()=>{if(entry.kind==='directory')go(entry.path);else open(entry.path);};list.append(button);
    }
    if(!result.entries.length)list.append(el('p','field-note','This directory is empty.'));
    if(result.truncated)list.append(el('p','field-note','Showing entries from the first 5,000 directory items.'));
  }
  const listUrl=(location,path)=>`/api/locations/${location.id}/files?path=${encodeURIComponent(path)}&hidden=${state.hidden}`;
  async function loadFiles(){
    const gen=++filesGeneration;const rootButton=el('button','text-button','Root');rootButton.onclick=()=>{state.path='';loadFiles();};$('file-breadcrumbs').replaceChildren(rootButton);$('files-error').textContent='';$('file-entries').replaceChildren(el('p','field-note','Loading directory…'));
    try{
      const location=await ensureLocation();if(gen!==filesGeneration)return;
      const result=await api(listUrl(location,state.path));
      if(gen!==filesGeneration)return;state.path=result.path;save();
      renderListing({crumbs:$('file-breadcrumbs'),list:$('file-entries'),result,selected:state.file?.path,go:path=>{state.path=path;loadFiles();},open:path=>openFile(location,path,false)});
    }catch(error){if(gen===filesGeneration){$('file-entries').replaceChildren();$('files-error').textContent=error.message;}}
  }
  // Side panel Files tab: browses the active session's directory, and shows a
  // file preview in place of the listing until "← Files".
  const side={location:null,path:'',generation:0};
  function sideListing(){$('side-file-browser').hidden=false;$('workspace-preview').hidden=true;}
  async function loadSideFiles(){
    const gen=++side.generation,location=side.location;sideListing();
    $('side-files-error').textContent='';$('side-file-entries').replaceChildren(el('p','field-note','Loading directory…'));
    try{
      const result=await api(listUrl(location,side.path));if(gen!==side.generation)return;side.path=result.path;
      renderListing({crumbs:$('side-file-crumbs'),list:$('side-file-entries'),result,selected:state.sideFile?.path,go:path=>{side.path=path;loadSideFiles();},open:path=>openFile(location,path,true)});
    }catch(error){if(gen===side.generation){$('side-file-entries').replaceChildren();$('side-files-error').textContent=error.message;}}
  }
  const sideFiles={
    show(session){
      if(side.location?.id!==session.locationId){side.location={id:session.locationId,path:session.cwd};side.path='';}
      if(state.sideFile&&state.sideFile.location.path!==session.cwd)state.sideFile=null;
      if(state.sideFile){$('side-file-browser').hidden=true;$('workspace-preview').hidden=false;renderPreview(state.sideFile,true);}else loadSideFiles();
    },
    hide(){side.generation++;},
  };
  // Files opened from chat links keep the session file endpoint, so forked
  // sessions still resolve references against their original directory.
  function fileUrl(file,download=false){
    const query=`file?path=${encodeURIComponent(file.path)}${download?'&download=true':''}`;
    const path=file.location.sessionId?`api/sessions/${file.location.sessionId}/${query}`:`api/locations/${file.location.id}/${query}`;
    return appUrl(path);
  }
  function localPath(href,base){
    if(!href||href.startsWith('#'))return null;
    if(/^file:/i.test(href)){try{const url=new URL(href);return ['','localhost'].includes(url.hostname)?decodeURIComponent(url.pathname):null;}catch{return null;}}
    if(/^[a-z][a-z0-9+.-]*:/i.test(href)||href.startsWith('//'))return null;
    const path=decodeURIComponent(href.split('#')[0]);return path.startsWith('/')?path:base+path;
  }
  function decorate(node,file,side){
    const base=file.path.includes('/')?file.path.slice(0,file.path.lastIndexOf('/')+1):'';
    for(const link of node.querySelectorAll('a')){
      const path=localPath(link.getAttribute('href'),base);
      if(path!==null){link.href='#';link.onclick=event=>{event.preventDefault();openFile(file.location,path,side);};}else if(/^file:/i.test(link.getAttribute('href')||''))link.removeAttribute('href');else{link.target='_blank';link.rel='noopener noreferrer';}
    }
    for(const img of node.querySelectorAll('img')){const path=localPath(img.getAttribute('src'),base);if(path!==null)img.src=fileUrl({...file,path});img.loading='lazy';}
  }
  function renderPreview(file,side){
    const area=side?$('workspace-preview'):$('workspace-file-detail');area.replaceChildren();
    const header=el('div','workspace-preview-header');header.append(el('strong','',file.path));
    const actions=el('div','workspace-preview-actions');const download=el('a','text-button','Download ↓');download.href=fileUrl(file,true);download.download=file.path.split('/').at(-1);actions.append(download);
    if(side){const back=el('button','text-button','← Files');back.onclick=()=>{state.sideFile=null;loadSideFiles();};actions.prepend(back);}header.append(actions);area.append(header);
    if(file.type?.startsWith('text/')){
      const tabs=el('div','workspace-preview-tabs');for(const [source,label]of [[false,'Preview'],[true,'Source']]){
        const button=el('button','text-button',label);button.setAttribute('aria-pressed',String(file.source===source));button.onclick=()=>{file.source=source;renderPreview(file,side);};tabs.append(button);
      }area.append(tabs);
    }
    const body=el('div','workspace-preview-body markdown');area.append(body);
    if(file.error)body.textContent=file.error;
    else if(file.loading)body.textContent='Loading file…';
    else if(file.type?.startsWith('image/')){const image=el('img','file-image');image.src=fileUrl(file);image.alt=file.path;body.append(image);}
    else if(file.type?.startsWith('text/')){
      if(!file.source&&file.html){body.innerHTML=file.html;decorate(body,file,side);enhanceMermaid(body,file.mermaidClass);}else body.append(el('pre','workspace-source',file.text.slice(0,100000)));
      hooks.enhanceBlocks?.(body,`workspace:${file.location.id}:${file.path}`,{language:!file.source&&file.html?'':/\.([\w+-]+)$/.exec(file.path)?.[1]||'',lineNumbers:file.source||!file.html,raw:file.source||!file.html?file.text:undefined});
      if(file.line&&file.source){
        const code=body.querySelector('pre code');
        if(code){
          // Wrap just the requested source line, retaining highlighted tokens and
          // the code block's exact raw copy buffer.
          const lines=file.text.split('\n'),target=Math.min(file.line,lines.length);
          const start=lines.slice(0,target-1).reduce((n,line)=>n+line.length+1,0),end=start+lines[target-1].length;
          const walker=document.createTreeWalker(code,NodeFilter.SHOW_TEXT),positions=[];let offset=0;
          while(walker.nextNode()){const node=walker.currentNode;positions.push({node,start:offset,end:offset+node.length});offset+=node.length;}
          const first=positions.find(p=>p.end>=start),last=positions.find(p=>p.end>=end);
          if(first&&last){
            const range=document.createRange();range.setStart(first.node,start-first.start);range.setEnd(last.node,end-last.start);
            const marker=el('span','referenced-line');marker.dataset.line=target;marker.setAttribute('aria-label',`Line ${target}`);marker.append(range.extractContents());range.insertNode(marker);
            requestAnimationFrame(()=>{if(marker.isConnected)marker.scrollIntoView({block:'center',behavior:'instant'});});
          }
        }
      }
      const limit=!file.source&&file.html?24000:100000;if(!file.line&&file.text.length>limit)body.append(el('p','field-note','Preview shortened. Download the file to read it in full.'));
    }else body.append(el('p','field-note','Preview is unavailable for this format. Use Download to save it.'));
    const footer=el('div','workspace-preview-footer'),ask=el('button','text-button','Ask about this file ↗');ask.disabled=!!file.error||file.loading;ask.onclick=()=>askAbout(file);footer.append(ask);
    if(!side){const beside=el('button','text-button','Open beside chat');beside.disabled=!!file.error||file.loading;beside.onclick=()=>{const active=hooks.getActive();if(!active){hooks.openCreate({projectId:state.projectId,cwd:file.location.path});return;}if(active.cwd!==file.location.path){hooks.toast('Choose a session in this directory, or start one with Ask about this file.');return;}state.sideFile=file;showView('sessions');hooks.openPanel('files');};footer.append(beside);}
    area.append(footer);
  }
  async function openFile(location,path,side=false,line=null){
    const file={location,path,line,source:!!line,text:'',type:'',loading:true};if(side){state.sideFile=file;$('side-file-browser').hidden=true;$('workspace-preview').hidden=false;}else{state.file=file;for(const node of $('file-entries').querySelectorAll('button'))node.classList.toggle('selected',node.dataset.path===path);save();}renderPreview(file,side);
    try{
      const response=await fetch(fileUrl(file));if(!response.ok)throw new Error(await response.text());
      file.type=response.headers.get('content-type')||'';
      if(file.type.startsWith('text/')){
        file.text=await response.text();
        if(/\.md$/i.test(file.path)){const result=await api('/api/markdown',{method:'POST',body:JSON.stringify({text:file.text.slice(0,24000)})});file.html=result.html;file.mermaidClass=result.mermaidClass;}
      }
    }catch(error){file.error=error.message;}finally{file.loading=false;if((side?state.sideFile:state.file)===file)renderPreview(file,side);}
  }
  function askAbout(file){
    const active=hooks.getActive(),text=`Please read the local file ${file.path} and help me understand it.`;
    if(active?.cwd===file.location.path){hooks.selectSession(active);hooks.appendDraft(text);state.sideFile=file;if(!narrow())hooks.openPanel('files');}
    else{pendingFile={cwd:file.location.path,text,file};hooks.openCreate({projectId:state.projectId,cwd:file.location.path});}
  }
  // A saved definition keeps its harness, even a hidden or retired one.
  function automationOptions(keep){
    const config=hooks.getConfig();if(!config)return;
    const items=config.providers.filter(p=>!p.hidden||p.id===keep).map(p=>[p.id,p.name]);
    if(keep&&!items.some(([id])=>id===keep))items.push([keep,hooks.labels[keep]||keep]);
    const previous=$('automation-provider').value;options($('automation-provider'),items,keep||(items.some(([id])=>id===previous)?previous:items[0]?.[0]));
    projectOptions($('automation-project'),$('automation-project').value);
  }
  function automationHosts(selected='local'){
    const items=[['local','Local'],...(catalog.hosts||[]).filter(h=>h.kind==='remote').map(h=>[h.id,h.name+(h.status!=='connected'?' · Offline':'')])];
    options($('automation-host'),items,selected);
  }
  function automationHostChanged(){
    editorGeneration++;const id=$('automation-host').value,active=hooks.getActive();
    $('automation-directory').value=active?.hostId===id?active.cwd:id==='local'?directory():(catalog.hosts.find(h=>h.id===id)?.roots?.[0]||'');
    $('automation-model').value='';$('automation-models').replaceChildren();automationProfiles($('automation-model-provider').value,false);
  }
  // Goose provider profiles enabled on the chosen host. A saved profile that is gone or not enabled
  // there stays selected and marked, so saving reports it instead of silently using the host environment.
  let profileGeneration=0,automationProfileList=[];
  const automationProfile=()=>$('automation-provider').value==='goose'?$('automation-model-provider').value||null:null;
  function automationProfileChanged(){const p=automationProfileList.find(p=>p.id===automationProfile());$('automation-model').placeholder=p?'Profile default · '+p.defaultModel:'Host default';}
  async function automationProfiles(selected,keepMissing=true){
    const gen=++profileGeneration,goose=$('automation-provider').value==='goose',select=$('automation-model-provider'),note=$('automation-model-provider-note');
    $('automation-model-provider-field').hidden=!goose;automationProfileList=[];
    select.replaceChildren(new Option('Host environment configuration',''),...(goose&&selected?[new Option('Loading saved provider…',selected)]:[]));select.value=goose&&selected||'';note.textContent='';
    if(!goose){automationProfileChanged();return;}
    select.disabled=true;
    try{
      const result=await api('/api/model-providers?hostId='+encodeURIComponent($('automation-host').value||'local'));if(gen!==profileGeneration)return;
      automationProfileList=result.profiles;const missing=selected&&!automationProfileList.some(p=>p.id===selected)&&keepMissing;
      select.replaceChildren(new Option('Host environment configuration',''),...automationProfileList.map(p=>new Option(`${p.name} · ${p.keyConfigured?'API key stored':'No API key stored'}`,p.id)),...(missing?[new Option('Unavailable profile · choose another',selected)]:[]));
      select.value=automationProfileList.some(p=>p.id===selected)||missing?selected:'';
      note.textContent=missing?'The saved provider profile was deleted or is not enabled on this host. Choose another before saving.':automationProfileList.length?'A profile replaces the host’s Goose environment for runs. Its key is only checked when a run contacts the provider.':'No provider profiles are enabled on this host. Add one in Settings → Model providers, or use the host environment.';
    }catch(error){if(gen===profileGeneration)note.textContent=error.message;}
    finally{if(gen===profileGeneration){select.disabled=false;automationProfileChanged();}}
  }
  // Recipe steps being edited. The server derives the run prompt from them; legacy definitions are one step.
  const MAX_STEPS=20;let steps=[];
  const stepsOf=def=>def?.workflow?.steps?.map(step=>step.text)||(def?.prompt?[def.prompt]:['']);
  function renderSteps(focus=null){
    const list=$('automation-steps');list.replaceChildren();
    steps.forEach((text,i)=>{
      const item=el('li','recipe-step'),input=el('textarea'),tools=el('div','recipe-step-tools');
      input.rows=3;input.required=true;input.maxLength=32768;input.value=text;input.placeholder=i?'Then…':'Describe what the agent should do first.';input.setAttribute('aria-label',`Step ${i+1} instructions`);input.oninput=()=>{steps[i]=input.value;};
      const tool=(label,glyph,disabled,action)=>{const button=el('button','icon-button',glyph);button.type='button';button.title=label;button.setAttribute('aria-label',`${label} step ${i+1}`);button.disabled=disabled;button.onclick=action;tools.append(button);};
      tool('Move up','↑',i===0,()=>moveStep(i,-1));tool('Move down','↓',i===steps.length-1,()=>moveStep(i,1));
      tool('Remove','×',steps.length===1,()=>{steps.splice(i,1);renderSteps(Math.min(i,steps.length-1));});
      item.append(el('span','recipe-step-number',String(i+1)),input,tools);list.append(item);
    });
    $('add-automation-step').disabled=steps.length>=MAX_STEPS;
    if(focus!==null)list.querySelectorAll('textarea')[focus]?.focus();
  }
  function moveStep(i,delta){const [step]=steps.splice(i,1);steps.splice(i+delta,0,step);renderSteps(i+delta);}
  // Weekly schedule trigger (A-01). Days are ISO weekdays, 1 = Monday.
  const DAYS=['Mon','Tue','Wed','Thu','Fri','Sat','Sun'];
  const triggerOf=def=>def?.workflow?.trigger||{kind:'manual'};
  const localZone=()=>{try{return Intl.DateTimeFormat().resolvedOptions().timeZone||'UTC';}catch{return 'UTC';}};
  function describeTrigger(trigger){
    if(trigger.kind==='once')return `Once on ${trigger.at.replace('T',' at ')} (${trigger.timeZone})${trigger.afterRun==='delete'?', then delete it':''}`;
    if(trigger.kind!=='schedule')return 'I click Run now';
    const days=trigger.days.length===7?'Every day':trigger.days.join()==='1,2,3,4,5'?'Weekdays':trigger.days.map(d=>DAYS[d-1]).join(', ');
    return `${days} at ${trigger.at} (${trigger.timeZone})`;
  }
  function triggerKindChanged(){const kind=$('automation-trigger').value;$('automation-schedule').hidden=kind==='manual';$('automation-days').hidden=kind!=='schedule';$('automation-date-field').hidden=$('automation-after-run-field').hidden=kind!=='once';}
  function scheduleFields(trigger){
    const scheduled=trigger.kind==='schedule',once=trigger.kind==='once',timed=scheduled||once;$('automation-trigger').value=trigger.kind;
    const tomorrow=new Date(Date.now()+86400000),pad=n=>String(n).padStart(2,'0');
    $('automation-date').value=once?trigger.at.slice(0,10):`${tomorrow.getFullYear()}-${pad(tomorrow.getMonth()+1)}-${pad(tomorrow.getDate())}`;$('automation-after-run').value=once?trigger.afterRun||'complete':'complete';
    $('automation-at').value=scheduled?trigger.at:once?trigger.at.slice(11):'09:00';$('automation-time-zone').value=timed?trigger.timeZone:localZone();$('automation-missed').value=timed?trigger.missed||'skip':'skip';triggerKindChanged();
    const days=scheduled?trigger.days:[1,2,3,4,5];for(const box of $('automation-days').querySelectorAll('input'))box.checked=days.includes(Number(box.value));
    const zones=$('automation-time-zones');if(!zones.children.length){let names=[];try{names=Intl.supportedValuesOf('timeZone');}catch{}for(const name of new Set(['UTC',...names])){const node=el('option');node.value=name;zones.append(node);}}
  }
  function triggerInput(){
    const kind=$('automation-trigger').value;if(kind==='manual')return {kind};
    if(kind==='once')return {kind,at:`${$('automation-date').value}T${$('automation-at').value}`,timeZone:$('automation-time-zone').value,missed:$('automation-missed').value,afterRun:$('automation-after-run').value};
    return {kind:'schedule',at:$('automation-at').value,timeZone:$('automation-time-zone').value,days:[...$('automation-days').querySelectorAll('input:checked')].map(box=>Number(box.value)),missed:$('automation-missed').value};
  }
  function recipeView(def){
    const box=el('div','automation-recipe-view'),when=el('p','recipe-row'),then=el('p','recipe-row'),list=el('ol','recipe-steps'),texts=stepsOf(def);
    when.append(el('span','recipe-word','When'),el('span','recipe-trigger',describeTrigger(triggerOf(def))));
    then.append(el('span','recipe-word','Do'),el('span','field-note',texts.length>1?`${texts.length} steps in order, in one agent turn`:'1 step'));
    for(const text of texts)list.append(el('li','automation-instructions',text));
    box.append(when,then,list);return box;
  }
  function automationEditor(def=null){
    editorGeneration++;editing=def?.id||null;automationOptions(def?.provider);automationHosts(def?.hostId||hooks.getActive()?.hostId||'local');$('automation-dialog-title').textContent=def?'Edit automation':'New automation';
    $('automation-name').value=def?.name||'';$('automation-model').value=def?.model||'';
    $('automation-project').value=def?.projectId||state.projectId||'';$('automation-directory').value=def?.cwd||(hooks.getActive()?.hostId===$('automation-host').value?hooks.getActive().cwd:$('automation-host').value==='local'?directory():catalog.hosts.find(h=>h.id===$('automation-host').value)?.roots?.[0]||'');steps=stepsOf(def);renderSteps();scheduleFields(triggerOf(def));$('automation-enabled').checked=def?.enabled??true;automationProfiles(def?.modelProvider||'');
    $('delete-automation').hidden=!def;$('automation-error').textContent='';$('automation-models').replaceChildren();$('automation-load-models').disabled=false;$('automation-dialog').showModal();
  }
  async function loadAutomations(){
    const gen=++automationGeneration;
    try{const list=await api('/api/automations');if(gen!==automationGeneration)return;definitions=list;renderAutomations();}
    catch(error){if(gen===automationGeneration)$('automation-list').replaceChildren(el('p','error-text',error.message));}
  }
  function visibleDefinitions(){return definitions.filter(d=>(d.projectId||null)===state.projectId);}
  function renderAutomations(){
    const list=$('automation-list');list.replaceChildren();const visible=visibleDefinitions();
    if(!visible.some(d=>d.id===state.automationId))state.automationId=visible[0]?.id||null;
    for(const def of visible){const button=el('button','automation-item'+(def.id===state.automationId?' selected':''));button.append(el('strong','',def.name),el('small','',`${hooks.labels[def.provider]} · ${def.completed?'Completed':def.enabled?{schedule:'Scheduled',once:'Once'}[triggerOf(def).kind]||'Manual':'Paused'} · ${stepsOf(def).length===1?'1 step':stepsOf(def).length+' steps'}`));button.onclick=()=>{state.automationId=def.id;save();renderAutomations();};list.append(button);}
    if(!visible.length)list.append(el('p','field-note','No automations in this project yet.'));
    const def=visible.find(d=>d.id===state.automationId);renderAutomation(def);if(def)loadRuns(def.id);
  }
  function renderAutomation(def){
    const area=$('automation-detail');area.replaceChildren();if(!def){area.append(el('div','workspace-empty','Save repeatable instructions, then start a manual run.'));return;}
    const header=el('div','automation-detail-header'),titles=el('div');const kind=triggerOf(def).kind,scheduled=kind!=='manual';titles.append(el('div','workspace-eyebrow',{schedule:'SCHEDULED AUTOMATION',once:'ONE-TIME AUTOMATION'}[kind]||'MANUAL AUTOMATION'),el('h1','',def.name));
    const actions=el('div','workspace-action-row'),edit=el('button','text-button','Edit'),run=el('button','primary','Run now ↗');edit.onclick=()=>automationEditor(def);run.id='run-automation';run.disabled=!def.enabled||runStarting;run.onclick=()=>runAutomation(def);actions.append(edit,run);header.append(titles,actions);area.append(header);
    const details=el('div','automation-execution');details.append(el('p','',`${hooks.labels[def.provider]} · ${def.model||'Host default'} · ${catalog.hosts.find(h=>h.id===def.hostId)?.name||def.hostId||'Local'}`));
    if(def.modelProvider){const status=def.modelProviderStatus||{};details.append(status.available?el('p','field-note',`Model provider: ${status.name} · ${status.keyConfigured?'API key stored (checked when a run contacts the provider)':'No API key stored'}`):el('p','error-text',`Model provider unavailable: ${status.detail||'unknown profile'}. Runs will not start until you choose another.`));}
    if(def.createdBy)details.append(el('p','field-note',`Created by agent: ${def.createdBy.title}. Scheduled runs need its current Manage automations and Start sessions grants.`));details.append(el('p','field-note',def.cwd),el('span','automation-badge',def.completed?'Completed '+new Date(def.completed).toLocaleString():!def.enabled?'Paused':!scheduled?'Manual trigger':def.nextRun?'Next run '+new Date(def.nextRun).toLocaleString():'No upcoming run'));area.append(details);
    area.append(el('h2','','Recipe'),recipeView(def),el('h2','','Recent runs'));
    const rows=el('div');rows.id='automation-runs';rows.append(el('p','field-note','Loading runs…'));area.append(rows);
  }
  async function loadRuns(id){
    try{const result=await api(`/api/automations/${id}/runs`);if(state.automationId!==id||!$('automation-runs'))return;runs=result;
      const area=$('automation-runs');area.replaceChildren();
      for(const run of runs){const row=el('button','automation-run');row.disabled=!run.sessionId;const title=el('span');title.append(el('strong','',run.scheduledFor?'Scheduled run':'Manual run'),el('small','',new Date(run.scheduledFor||run.created).toLocaleString()));if(run.detail)title.append(el('small','',run.detail));row.append(title,el('span','run-status '+run.status,run.status==='waiting'?'Needs approval':run.status));row.onclick=async()=>{try{const snapshot=await api('/api/sessions/'+run.sessionId);hooks.selectSession(snapshot.session);}catch(error){hooks.toast(error.message);}};area.append(row);}
      if(!runs.length)area.append(el('p','field-note','No runs yet. Run now starts a new agent session.'));
      const button=$('run-automation');if(button)button.disabled=runStarting||!definitions.find(d=>d.id===id)?.enabled||runs.some(r=>['starting','resuming','running','waiting','cancelling','configuring'].includes(r.status));
    }catch(error){if(state.automationId===id&&$('automation-runs'))$('automation-runs').textContent=error.message;}
  }
  async function runAutomation(def){
    if(runStarting)return;runStarting=true;$('run-automation').disabled=true;
    pendingRuns[def.id]??=hooks.uuid();sessionStorage.setItem('agentworks-pending-runs',JSON.stringify(pendingRuns));
    try{const result=await api(`/api/automations/${def.id}/run`,{method:'POST',body:JSON.stringify({requestId:pendingRuns[def.id]})});delete pendingRuns[def.id];sessionStorage.setItem('agentworks-pending-runs',JSON.stringify(pendingRuns));await hooks.refreshSessions();hooks.selectSession(result.session);}
    catch(error){if(error.status===410){delete pendingRuns[def.id];sessionStorage.setItem('agentworks-pending-runs',JSON.stringify(pendingRuns));}hooks.toast(error.message);}finally{runStarting=false;if(state.view==='automations')loadRuns(def.id);}
  }
  function sessionSelected(session){
    if(session.locationId&&!catalog.locations.some(l=>l.id===session.locationId))catalog.locations.push({id:session.locationId,hostId:session.hostId||'local',path:session.cwd});
    const project=catalog.projects.find(p=>p.id===session.projectId);if(project&&!project.locationIds?.includes(session.locationId))(project.locationIds??=[]).push(session.locationId);
    if(state.locationKey!==session.locationId)resetFiles();
    state.projectId=session.projectId||null;state.locationKey=session.locationId;contextControls();showView('sessions');
    if(state.sideFile&&state.sideFile.location.path!==session.cwd)state.sideFile=null;
    const source=$('automation-source');source.hidden=!session.automation;source.textContent=session.automation?'Automation · '+session.automation.name:'';
    if(pendingFile?.cwd===session.cwd){const pending=pendingFile;pendingFile=null;hooks.appendDraft(pending.text);state.sideFile=pending.file;if(!narrow())hooks.openPanel('files');}
  }
  // Below this width the side panel replaces the chat rather than sitting beside it.
  const narrow=()=>matchMedia('(max-width:800px)').matches;
  function openAutomation(id){state.automationId=id;showView('automations');}
  // On phones the Sessions tab is the full-screen session browser.
  $('nav-sessions').onclick=()=>{showView('sessions');if(matchMedia('(max-width:650px)').matches)document.body.classList.add('menu-open');};$('nav-files').onclick=()=>showView('files');$('nav-automations').onclick=()=>showView('automations');$('nav-settings').onclick=()=>hooks.openSettings();
  $('workspace-project').onchange=()=>{contextGeneration++;state.projectId=$('workspace-project').value||null;state.locationKey='';contextControls();resetFiles();save();
    if(state.view==='sessions'){const session=sessions.find(s=>(s.projectId||null)===state.projectId);if(session)hooks.selectSession(session);else hooks.overview();}
    else if(state.view==='files')loadFiles();else loadAutomations();
  };
  $('workspace-location').onchange=()=>{contextGeneration++;state.locationKey=$('workspace-location').value;resetFiles();save();
    if(state.view==='sessions'){const session=sessions.find(s=>s.cwd===directory()&&(s.projectId||null)===state.projectId);if(session)hooks.selectSession(session);else hooks.overview();}else if(state.view==='files')loadFiles();
  };
  $('add-location').onclick=()=>{$('location-path').value=directory();$('location-error').textContent='';$('location-dialog').showModal();};
  $('dismiss-location').onclick=()=>$('location-dialog').close();$('location-form').onsubmit=async event=>{event.preventDefault();$('save-location').disabled=true;
    try{const location=await api('/api/locations',{method:'POST',body:JSON.stringify({cwd:$('location-path').value,projectId:state.projectId})});await hooks.refreshSessions();state.locationKey=location.id;contextControls();resetFiles();$('location-dialog').close();showView('files');}
    catch(error){$('location-error').textContent=error.message;}finally{$('save-location').disabled=false;}
  };
  $('refresh-files').onclick=loadFiles;
  // Both file browsers share the hidden-files flag, so the two checkboxes stay in step.
  const setHiddenFiles=checked=>{state.hidden=checked;$('files-hidden').checked=checked;$('side-files-hidden').checked=checked;};
  $('files-hidden').onchange=()=>{setHiddenFiles($('files-hidden').checked);loadFiles();};
  $('side-files-hidden').onchange=()=>{setHiddenFiles($('side-files-hidden').checked);loadSideFiles();};
  $('automation-source').onclick=()=>{const origin=hooks.getActive()?.automation;if(origin)openAutomation(origin.automationId);};
  $('new-automation').onclick=()=>automationEditor();$('dismiss-automation').onclick=()=>$('automation-dialog').close();
  $('add-automation-step').onclick=()=>{if(steps.length>=MAX_STEPS)return;steps.push('');renderSteps(steps.length-1);};
  $('automation-host').onchange=automationHostChanged;
  $('automation-trigger').onchange=triggerKindChanged;
  $('automation-provider').onchange=()=>{editorGeneration++;$('automation-model').value='';$('automation-models').replaceChildren();automationProfiles('');};
  $('automation-model-provider').onchange=()=>{editorGeneration++;$('automation-models').replaceChildren();automationProfileChanged();};$('automation-directory').oninput=()=>{editorGeneration++;};
  $('automation-load-models').onclick=async()=>{const token=editorGeneration;const button=$('automation-load-models');button.disabled=true;$('automation-error').textContent='';
    try{const result=await api('/api/models',{method:'POST',body:JSON.stringify({provider:$('automation-provider').value,modelProvider:automationProfile(),cwd:$('automation-directory').value,...($('automation-host').value!=='local'?{hostId:$('automation-host').value}:{})})});if(token!==editorGeneration)return;$('automation-models').replaceChildren();for(const model of result.options||[]){const node=el('option');node.value=model.value;node.label=model.name||model.value;$('automation-models').append(node);}}
    catch(error){if(token===editorGeneration)$('automation-error').textContent=error.message;}finally{button.disabled=false;}
  };
  $('automation-form').onsubmit=async event=>{event.preventDefault();$('save-automation').disabled=true;$('automation-error').textContent='';
    try{const def=await api(editing?'/api/automations/'+editing:'/api/automations',{method:editing?'PUT':'POST',body:JSON.stringify({name:$('automation-name').value,provider:$('automation-provider').value,model:$('automation-model').value||null,...(automationProfile()?{modelProvider:automationProfile()}:{}),projectId:$('automation-project').value||null,...($('automation-host').value!=='local'?{hostId:$('automation-host').value}:{}),cwd:$('automation-directory').value,workflow:{version:1,trigger:triggerInput(),steps:steps.map(text=>({kind:'agent',text}))},enabled:$('automation-enabled').checked})});state.projectId=def.projectId;state.locationKey=def.locationId;state.automationId=def.id;await hooks.refreshSessions();$('automation-dialog').close();showView('automations');}
    catch(error){$('automation-error').textContent=error.message;}finally{$('save-automation').disabled=false;}
  };
  $('delete-automation').onclick=async()=>{if(!editing||!confirm('Delete this automation definition? Its run sessions and history will remain.'))return;try{await api('/api/automations/'+editing,{method:'DELETE'});$('automation-dialog').close();loadAutomations();}catch(error){$('automation-error').textContent=error.message;}};
  return {
    sync(next,list){catalog=next;sessions=list;contextControls();if(state.view==='automations')loadAutomations();},
    sessionSelected,showView,sideFiles,
    // Chat file links open in the side panel's Files tab.
    openSideFile(session,path,line=null){openFile({id:session.locationId,path:session.cwd,sessionId:session.id},path,true,line);hooks.openPanel('files');},
    context(){return {projectId:state.projectId,cwd:directory()};},
    restoreView(){if(initial.view&&initial.view!=='sessions'){state.projectId=initial.projectId||null;state.locationKey=initial.locationKey||'';state.path=initial.path||'';contextControls();showView(initial.view);if(initial.view==='files'&&initial.filePath){const expected=directory();ensureLocation().then(location=>{if(state.view==='files'&&directory()===expected&&!state.file)openFile(location,initial.filePath);}).catch(()=>{});}}},
    rootsChanged(){contextControls();if(state.view==='files')loadFiles();},
  };
}
