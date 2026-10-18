import {setupDiff} from '/diff.js';
import {setupWorktreeControls,setupPolicyEditor} from '/worktree-controls.js';
import {setupGitContext} from '/git-context.js';
import {createWorkspace} from '/workspace.js';
import {setupCommunication} from '/communication.js';
import {setupPalettes} from '/palettes.js';
import {setupDisplayPreferences} from '/display-preferences.js';
const $ = id => document.getElementById(id);
// randomUUID is unavailable on plain HTTP remote origins; getRandomValues is supported.
const uuid = () => globalThis.crypto.randomUUID?.() ?? Array.from(crypto.getRandomValues(new Uint8Array(16)), byte => byte.toString(16).padStart(2, '0')).join('');
const labels = {demo:'Demo agent',claude:'Claude',codex:'Codex',opencode:'OpenCode'};
const icons = {demo:'◈',claude:'✳',codex:'⌘',opencode:'▣'};
function readStorage(storage,key,fallback){try{return JSON.parse(storage.getItem(key))??fallback;}catch{return fallback;}}
let workspace;
let projects=[],hosts=[{id:'local',name:'Local',status:'connected'}];
let sidebarView=localStorage.getItem('agentworks-sidebar-view')==='hosts'?'hosts':'projects';
let collapsedGroups=new Set(readStorage(localStorage,'agentworks-tree-collapsed',[]));
let editingProject=null,savingProject=false;
const drafts=readStorage(sessionStorage,'agentworks-drafts',{});
const scrollPositions=new Map();let restoreScroll=null;
let config, sessions=[], active=null, socket=null, generation=0, connected=false;
let transcript=[], tools=new Map(), approvals=new Map(), assistant=null, status='starting';
let modelConfig=null, availableCommands=[], commandsOpen=false;
let discoveryGeneration=0;
let externalSessions=[], externalSelection=null, externalCursor=null, externalGeneration=0, externalLoading=false, externalImporting=false;
let drawTimer=null, retryTimer=null, reconnectDelay=500, pendingSend=null, creating=false;
const nodes=new Map();
let pendingCreate=readStorage(sessionStorage,'agentworks-pending-create',null);
const worktrees=setupWorktreeControls({$,api,onChange:()=>{if(config)providerNote();}});
const policyEditor=setupPolicyEditor({$,api});
const display=setupDisplayPreferences();display.mount($('display-preferences'));

async function api(path, options={}) {
  const response=await fetch(path,{...options,headers:{'Content-Type':'application/json',...options.headers}});
  if(!response.ok){const error=new Error(await response.text());error.status=response.status;throw error;}
  return response.json();
}
// tone: 'error' (default) or 'success'; success uses a polite status region.
function toast(message,tone='error') {
  const node=$('toast');node.hidden=true;node.dataset.tone=tone==='success'?'success':'error';node.setAttribute('role',tone==='success'?'status':'alert');
  node.textContent=message;node.hidden=false;
  clearTimeout(toast.timer);toast.timer=setTimeout(()=>$('toast').hidden=true,6500);
}
function el(tag,className,text) {const node=document.createElement(tag);if(className)node.className=className;if(text!==undefined)node.textContent=text;return node;}
function basename(path) {return path.replaceAll('\\','/').split('/').filter(Boolean).at(-1)||path;}
function providerNote() {
  const p=config.providers.find(p=>p.id===$('provider').value);
  $('provider-note').textContent=p.id==='demo'?'Local test agent. No model calls, API key, or file changes.':p.id==='opencode'?(p.installed?'Native ACP. Choose an advertised free model. Availability and limits may change.':'Install OpenCode on this host or set AGENTWORKS_OPENCODE_PATH.'):!p.installed?'Adapter not installed. Run npm ci --ignore-scripts.':p.authMode==='existing'?'Uses the existing agent sign-in on this host. If it has expired, sign in with the CLI and try again.':!p.keyPresent?`Set ${p.id==='claude'?'ANTHROPIC_API_KEY':'OPENAI_API_KEY or CODEX_API_KEY'} before starting the server.`:'API key available on the host. Messages use your provider account.';
  $('create-submit').disabled=!p.installed||(p.authMode==='api_key'&&!p.keyPresent)||creating||worktrees.blocked();
}
function openCreate(context={}) {
  if(!config)return;
  updateProjectControls();
  const workspaceContext=workspace?.context();
  const projectId=Object.hasOwn(context,'projectId')?context.projectId:workspaceContext?.projectId??active?.projectId??null;
  $('create-project').value=projectId||'';
  const locations=readStorage(localStorage,'agentworks-project-locations',{});
  $('create-resume-policy').checked=false;
  $('create-agent-access').value='off';$('create-error').textContent='';$('directory').value=context.cwd||workspaceContext?.cwd||locations[projectId||'ungrouped']||active?.cwd||config.roots[0];$('session-title').value='';
  resetCreateModels();worktrees.projectChanged();providerNote();$('create-dialog').showModal();
}
function resetCreateModels(){
  discoveryGeneration++;
  const select=$('create-model');select.replaceChildren();
  const defaultId=config.defaultModels?.[$('provider').value];
  const option=el('option','',defaultId?`Saved default · ${defaultId}`:'Harness default');option.value=defaultId||'';select.append(option);
  $('load-models').disabled=false;$('model-note').textContent='Load available models from the agent without sending a prompt.';
}
async function loadCreateModels(){
  const gen=++discoveryGeneration,provider=$('provider').value,cwd=$('directory').value;
  $('load-models').disabled=true;$('model-note').textContent='Asking the harness for models…';
  try{
    const result=await api('/api/models',{method:'POST',body:JSON.stringify({provider,cwd})});
    if(gen!==discoveryGeneration)return;
    const select=$('create-model'),previous=select.value;select.replaceChildren();
    const def=el('option','','Harness default');def.value='';select.append(def);
    for(const model of result.options||[]){const option=el('option','',`${model.name||model.value} · ${model.value}`);option.value=model.value;select.append(option);}
    select.value=[...select.options].some(o=>o.value===previous)?previous:'';
    $('model-note').textContent=result.options?.length?'Model is applied before the first turn.':'This harness did not advertise selectable models.';
  }catch(e){if(gen===discoveryGeneration)$('model-note').textContent=e.message;}
  finally{if(gen===discoveryGeneration)$('load-models').disabled=false;}
}
function commandText(command){return command.name.startsWith('$')?command.name:'/'+command.name.replace(/^\//,'');}
function renderCommands(){
  const list=$('command-list');list.replaceChildren();list.hidden=!commandsOpen;
  if(!commandsOpen)return;
  const query=$('prompt').value.trim().toLowerCase();
  const items=availableCommands.filter(c=>!/^[/\$]\S*$/.test(query)||commandText(c).toLowerCase().includes(query));
  if(!items.length)list.append(el('p','field-note','No matching commands advertised by this harness.'));
  for(const command of items){
    const button=el('button','',commandText(command));button.type='button';button.append(el('small','',command.description||''));
    button.onclick=()=>{$('prompt').value=commandText(command)+' ';commandsOpen=false;renderCommands();$('prompt').dispatchEvent(new Event('input'));$('prompt').focus();};list.append(button);
  }
}
function openImport(){
  if(!config)return;
  $('create-dialog').close();updateProjectControls();
  $('import-provider').replaceChildren();
  for(const provider of config.providers){const option=el('option','',provider.name);option.value=provider.id;option.disabled=!provider.installed;$('import-provider').append(option);}
  $('import-provider').value=active?.provider||'claude';$('import-project').value=active?.projectId||'';
  $('import-directory').value='';$('import-search').value='';resetExternal();$('import-dialog').showModal();
}
function resetExternal(){
  externalGeneration++;externalSessions=[];externalSelection=null;externalCursor=null;externalLoading=false;externalImporting=false;
  $('import-error').textContent='';$('import-note').textContent='History and agent context will be loaded.';
  $('import-source-field').hidden=$('import-provider').value!=='opencode';
  $('external-sessions').replaceChildren(el('p','field-note','Choose an agent, then Browse.'));externalControls();
}
function externalControls(){
  $('browse-sessions').disabled=externalLoading||externalImporting;$('browse-sessions').textContent=externalLoading?'Loading…':'Browse';
  $('more-external').hidden=!externalCursor;$('more-external').disabled=externalLoading||externalImporting;
  $('import-submit').disabled=!externalSelection||externalLoading||externalImporting;
  for(const id of ['import-provider','import-source','import-directory','import-project','import-search'])$(id).disabled=externalImporting;
}
function renderExternal(){
  const list=$('external-sessions');list.replaceChildren();
  const query=$('import-search').value.trim().toLowerCase();
  const items=externalSessions.filter(s=>[s.title,s.cwd,s.sessionId].join(' ').toLowerCase().includes(query));
  if(!items.length)list.append(el('p','field-note',externalLoading?'Loading sessions…':externalCursor?'No matching sessions on this page. Try Load more.':'No matching sessions. Check allowed roots or try another directory.'));
  for(const session of items){
    const label=el('label','external-session'),radio=el('input');radio.type='radio';radio.name='external-session';radio.value=session.sessionId;radio.checked=externalSelection?.sessionId===session.sessionId;radio.disabled=externalImporting;
    radio.onchange=()=>{externalSelection=session;$('import-note').textContent=session.agentworksId?(session.archived?'Archived in Agentworks. Opens the saved conversation.':'Already in Agentworks. Opens the existing conversation.'):'History and agent context will be loaded.';externalControls();};
    const content=el('span','external-description');content.append(el('strong','',session.title||'Untitled session'),el('small','',session.cwd));
    const date=session.updatedAt?new Date(session.updatedAt):null;
    content.append(el('small','external-id',(date&&!Number.isNaN(+date)?date.toLocaleString()+' · ':'')+session.sessionId));
    if(session.agentworksId)content.append(el('small','',session.archived?'Archived in Agentworks':'Already in Agentworks'));
    label.append(radio,content);list.append(label);
  }
}
async function browseExternal(more=false){
  const gen=++externalGeneration,provider=$('import-provider').value,cwd=$('import-directory').value.trim(),hostProfile=$('import-source').value==='host';
  if(!more){externalSessions=[];externalSelection=null;externalCursor=null;}
  externalLoading=true;$('import-error').textContent='';externalControls();renderExternal();
  try{
    const result=await api('/api/external-sessions',{method:'POST',body:JSON.stringify({provider,cwd:cwd||null,cursor:more?externalCursor:null,hostProfile})});
    if(gen!==externalGeneration)return;
    const seen=new Set(externalSessions.map(s=>s.sessionId));for(const session of result.sessions||[])if(!seen.has(session.sessionId)){externalSessions.push(session);seen.add(session.sessionId);}
    externalCursor=result.nextCursor||null;
  }catch(e){if(gen===externalGeneration)$('import-error').textContent=e.message;}
  finally{if(gen===externalGeneration){externalLoading=false;renderExternal();externalControls();}}
}
$('import-session').onclick=openImport;$('create-import').onclick=openImport;
$('dismiss-import').onclick=()=>{$('import-dialog').close();externalGeneration++;};
$('import-provider').onchange=resetExternal;$('import-source').onchange=resetExternal;$('import-directory').oninput=resetExternal;
$('import-search').oninput=renderExternal;$('browse-sessions').onclick=()=>browseExternal();$('more-external').onclick=()=>browseExternal(true);
$('import-form').onsubmit=async event=>{
  event.preventDefault();if(!externalSelection||externalImporting||externalLoading)return;
  const selection=externalSelection;externalImporting=true;externalControls();$('import-error').textContent='';
  try{
    const created=await api('/api/sessions/import',{method:'POST',body:JSON.stringify({provider:$('import-provider').value,providerSessionId:selection.sessionId,cwd:selection.cwd,title:selection.title||'Imported session',projectId:$('import-project').value||null,hostProfile:$('import-source').value==='host'})});
    await refreshSessions();$('import-dialog').close();selectSession(created);
    if(created.status==='error')resumeSession(created.id);
  }catch(e){$('import-error').textContent=e.message;}finally{externalImporting=false;externalControls();}
};
function projectName(id){return projects.find(p=>p.id===id)?.name||'Ungrouped';}
function updateProjectControls(){
  const ordered=[...projects].sort((a,b)=>a.name.localeCompare(b.name));
  for(const id of ['create-project','session-project','import-project']){
    const select=$(id), signature=JSON.stringify(ordered), current=id==='session-project'?active?.projectId:select.value;
    if(select.dataset.projects!==signature){
      select.replaceChildren();const ungrouped=el('option','','Ungrouped');ungrouped.value='';select.append(ungrouped);
      for(const project of ordered){const option=el('option','',project.name);option.value=project.id;select.append(option);}
      select.dataset.projects=signature;
    }
    select.value=current||'';
  }
}
function setSidebarView(view){
  sidebarView=view;localStorage.setItem('agentworks-sidebar-view',view);sidebar();
}
function badges(items){
  const wrap=el('span','group-badges');
  const running=items.filter(s=>['running','cancelling','starting','resuming','configuring'].includes(s.status)).length;
  const waiting=items.filter(s=>s.status==='waiting').length;
  if(running)wrap.append(el('span','group-count running',`${running} running`));
  if(waiting)wrap.append(el('span','group-count waiting',`${waiting} waiting`));
  if(!running&&!waiting&&items.length)wrap.append(el('span','group-total',String(items.length)));
  return wrap;
}
function treeGroup(key,label,items,{host=false,projectId=null,level=0}={}){
  const group=el('details','tree-group level-'+level);group.dataset.group=key;
  const stateKey=sidebarView+':'+key;group.open=!collapsedGroups.has(stateKey);
  const heading=el('summary','tree-heading');heading.append(el('span','tree-chevron','›'));
  heading.append(el('span',host?'tree-icon host':'tree-icon',host?'▤':'◇'),el('span','tree-label',label),badges(items));
  if(host){const dot=el('span','connection-dot');dot.title='Connected';heading.append(dot);}
  if(!host&&projectId){
    const rename=el('button','group-action','⋯');rename.type='button';rename.setAttribute('aria-label','Edit project '+label);
    rename.onclick=event=>{event.preventDefault();event.stopPropagation();openProjectDialog(projects.find(p=>p.id===projectId));};heading.append(rename);
  }
  const add=el('button','group-action add-session','＋');add.type='button';add.setAttribute('aria-label','New session in '+label);
  add.onclick=event=>{event.preventDefault();event.stopPropagation();openCreate({projectId});};heading.append(add);
  const children=el('div','tree-children');group.append(heading,children);
  group.ontoggle=()=>{if(group.open)collapsedGroups.delete(stateKey);else collapsedGroups.add(stateKey);localStorage.setItem('agentworks-tree-collapsed',JSON.stringify([...collapsedGroups]));};
  return {group,children};
}
function sessionButton(s){
  const button=el('button','session-item'+(active?.id===s.id?' selected':''));button.type='button';button.dataset.sessionId=s.id;button.title=s.cwd;
  button.append(el('span','session-icon',icons[s.provider]));
  const copy=el('span','session-copy');copy.append(el('strong','',s.title),el('small','',`${s.archived?'Archived · ':''}${s.automation?'Automation · ':''}${labels[s.provider]} · ${basename(s.cwd)}`));
  button.append(copy,el('span','status-dot '+s.status));button.onclick=()=>selectSession(s);
  if(active?.id===s.id)button.setAttribute('aria-current','true');return button;
}
function sidebar(){
  const visibleSessions=sessions.filter(s=>$('session-filter').value==='all'||!!s.archived===($('session-filter').value==='archived'));
  $('session-count').textContent=visibleSessions.length;
  for(const view of ['hosts','projects'])$('view-'+view).setAttribute('aria-selected',String(sidebarView===view));
  const area=$('sessions'),scroll=area.scrollTop;area.replaceChildren();
  const buckets=[...projects.map(p=>({id:p.id,name:p.name})).sort((a,b)=>a.name.localeCompare(b.name)),{id:null,name:'Ungrouped'}];
  function addSessions(container,items){
    for(const session of items)container.append(sessionButton(session));
    if(!items.length)container.append(el('div','empty-group','No sessions yet'));
  }
  if(sidebarView==='hosts'){
    for(const host of hosts){
      const hostItems=visibleSessions.filter(s=>s.hostId===host.id);
      const top=treeGroup('host:'+host.id,host.name,hostItems,{host:true});
      for(const project of buckets){
        const items=hostItems.filter(s=>(s.projectId||null)===project.id);
        if(project.id===null&&!items.length&&projects.length)continue;
        const child=treeGroup(`host:${host.id}:project:${project.id||'ungrouped'}`,project.name,items,{projectId:project.id,level:1});
        addSessions(child.children,items);top.children.append(child.group);
      }
      area.append(top.group);
    }
  }else{
    for(const project of buckets){
      const projectItems=visibleSessions.filter(s=>(s.projectId||null)===project.id);
      if(project.id===null&&!projectItems.length&&projects.length)continue;
      const top=treeGroup('project:'+(project.id||'ungrouped'),project.name,projectItems,{projectId:project.id});
      for(const host of hosts){
        const items=projectItems.filter(s=>s.hostId===host.id);
        const child=treeGroup(`project:${project.id||'ungrouped'}:host:${host.id}`,host.name,items,{host:true,projectId:project.id,level:1});
        addSessions(child.children,items);top.children.append(child.group);
      }
      area.append(top.group);
    }
  }
  area.scrollTop=scroll;updateProjectControls();
}
async function refreshSessions(){
  const [list,catalog]=await Promise.all([api('/api/sessions?archived=all'),api('/api/catalog')]);
  sessions=list;projects=catalog.projects;hosts=catalog.hosts;
  if(active){const current=sessions.find(s=>s.id===active.id);if(current){active.projectId=current.projectId;active.hostId=current.hostId;active.automation=current.automation;}}
  sidebar();workspace?.sync(catalog,sessions);
}
function openProjectDialog(project=null){
  if(savingProject)return;
  editingProject=project?.id||null;$('project-dialog-title').textContent=project?'Edit project':'New project';
  $('project-name').value=project?.name||'';$('project-error').textContent='';$('delete-project').hidden=!project;
  policyEditor.open(editingProject);$('project-dialog').showModal();$('project-name').focus();
}
function resetTranscript(){transcript=[];tools=new Map();approvals=new Map();assistant=null;nodes.clear();$('messages').replaceChildren();$('approval-area').replaceChildren();}
function selectSession(s) {
  document.body.classList.remove('menu-open');
  if(active?.id===s.id){$('welcome').hidden=true;$('conversation').hidden=false;workspace?.sessionSelected(s);return;}
  if(active){drafts[active.id]=$('prompt').value;sessionStorage.setItem('agentworks-drafts',JSON.stringify(drafts));scrollPositions.set(active.id,$('messages').scrollTop);}
  $('prompt').value=drafts[s.id]||'';restoreScroll=scrollPositions.get(s.id)??null;
  clearTimeout(retryTimer);generation++;socket?.close();socket=null;connected=false;
  active=s;status=s.status;pendingSend=null;modelConfig=null;availableCommands=[];commandsOpen=false;renderCommands();resetTranscript();
  localStorage.setItem('agentworks-session',s.id);$('welcome').hidden=true;$('conversation').hidden=false;
  $('header-title').textContent=s.title;$('provider-label').textContent=labels[s.provider];$('project-path').textContent=s.cwd;
  $('session-notice').hidden=true;document.body.classList.remove('menu-open');sidebar();updateControls();connect(generation);
  workspace?.sessionSelected(s);
  if(s.status==='suspended'&&!s.explicitlySuspended)resumeSession(s.id);
}
function connect(gen){
  if(gen!==generation||!active)return;
  $('connection').textContent='Connecting';
  const ws=new WebSocket(`${location.protocol==='https:'?'wss':'ws'}://${location.host}/api/sessions/${active.id}/events`);socket=ws;
  ws.onopen=()=>{if(gen!==generation)return;connected=true;reconnectDelay=500;$('connection').textContent='Connected';updateControls();};
  ws.onmessage=event=>{
    if(gen!==generation)return;
    const e=JSON.parse(event.data);
    if(e.kind==='snapshot'){
      resetTranscript();status=e.session.status;Object.assign(active,e.session);
      for(const item of e.events)if(item.kind!=='session_updated')consume(item);
      Object.assign(active,e.session);$('header-title').textContent=active.title;
      active.continuation=e.session.continuation;
      approvals=new Map(e.approvals.map(a=>[a.id,a.request]));status=e.session.status;modelConfig=e.models;if(status==='suspended'&&!e.session.explicitlySuspended)resumeSession(active.id);availableCommands=e.commands||[];renderCommands();
      if(e.storageError){$('session-notice').textContent=e.storageError;$('session-notice').hidden=false;}
      if(e.truncated){$('session-notice').textContent='Showing recent output. The full transcript is saved on this host.';$('session-notice').hidden=false;}
    }else consume(e);
    scheduleDraw();updateControls();renderContinuation();
  };
  ws.onclose=()=>{
    if(gen!==generation)return;
    connected=false;$('connection').textContent='Reconnecting';updateControls();
    const retry=async()=>{
      if(gen!==generation||!active)return;
      try{await api(`/api/sessions/${active.id}`);connect(gen);}catch(error){
        if(gen!==generation)return;
        if(error.status===404){
          $('session-notice').textContent='This session was deleted.';$('session-notice').hidden=false;
          status='closed';updateControls();refreshSessions().catch(()=>{});
        }else{retryTimer=setTimeout(retry,reconnectDelay);reconnectDelay=Math.min(5000,reconnectDelay*2);}
      }
    };
    retryTimer=setTimeout(retry,reconnectDelay);reconnectDelay=Math.min(5000,reconnectDelay*2);
  };
}
function addMessage(role,text){const message={id:uuid(),type:'message',role,text};transcript.push(message);return message;}
function consume(e){
  const d=e.data;
  if(d?.continuationState&&active)active.continuation=d.continuationState;
  if(e.kind==='session_updated'){
    Object.assign(active,d);$('header-title').textContent=active.title;const row=sessions.find(s=>s.id===active.id);if(row)Object.assign(row,d);sidebar();
  }
  else if(e.kind==='continuation'){renderContinuation();}
  else if(e.kind==='models'){modelConfig=d;}
  else if(e.kind==='commands'){availableCommands=Array.isArray(d)?d:[];renderCommands();}
  else if(e.kind==='user'){assistant=null;if(d.initiatedBy)transcript.push({id:uuid(),type:'notice',text:'Turn started by agent session '+(sessions.find(s=>s.id===d.initiatedBy)?.title||d.initiatedBy)});addMessage('user',d.text);}
  else if(e.kind==='status'){
    status=d.status;if(active){active.status=status;if(d.explicitlySuspended!==undefined)active.explicitlySuspended=d.explicitlySuspended;const s=sessions.find(s=>s.id===active.id);if(s)s.status=status;}sidebar();
  }else if(e.kind==='approval'){approvals.set(d.id,d.request);}
  else if(e.kind==='approval_resolved'){approvals.delete(d.id);}
  else if(e.kind==='interrupted'){transcript.push({id:uuid(),type:'notice',text:d.message});assistant=null;}
  else if(e.kind==='storage_error'){status='error';$('session-notice').textContent=d.message;$('session-notice').hidden=false;}
  else if(e.kind==='error'){transcript.push({id:uuid(),type:'error',text:d.message});assistant=null;}
  else if(e.kind==='turn_end'){assistant=null;}
  else if(e.kind==='acp'){
    switch(d.sessionUpdate){
      case 'user_message_chunk':{
        assistant=null;if(d.content?.type==='text'){
          const last=transcript.at(-1);
          if(last?.replayed&&last.role==='user'&&last.providerMessageId===d.messageId)last.text+=d.content.text;
          else{const message=addMessage('user',d.content.text);message.replayed=true;message.providerMessageId=d.messageId;}
        }break;
      }
      case 'agent_message_chunk':{
        if(d.content?.type==='text'){
          if(!assistant||(d.messageId&&assistant.providerMessageId&&d.messageId!==assistant.providerMessageId))assistant=addMessage('assistant','');assistant.providerMessageId=d.messageId;assistant.text+=d.content.text;
        }else if(d.content?.type==='image'){
          transcript.push({id:uuid(),type:'image',content:d.content});assistant=null;
        }else if(d.content?.type==='resource_link'){
          assistant=null;addMessage('assistant',`[${d.content.name||'Open file'}](${d.content.uri})`);
        }break;
      }
      case 'tool_call':{
        assistant=null;const tool={...d,id:d.toolCallId,type:'tool'};tools.set(d.toolCallId,tool);transcript.push(tool);break;
      }
      case 'tool_call_update':{
        let tool=tools.get(d.toolCallId);
        if(!tool){tool={id:d.toolCallId,type:'tool',title:'Tool activity'};tools.set(d.toolCallId,tool);transcript.push(tool);}Object.assign(tool,d,{type:'tool'});break;
      }
      case 'plan':{
        const text=(d.entries||[]).map(p=>`- ${p.status==='completed'?'✓':'○'} ${p.content}`).join('\n');
        transcript.push({id:uuid(),type:'plan',text});break;
      }
      default:break;
    }
  }
}
async function resumeSession(id){
  try{await api(`/api/sessions/${id}/resume`,{method:'POST',body:'{}'});}catch(e){toast(e.message);}
}
function updateControls(){
  $('archive-session').textContent=active?.archived?'Unarchive session':'Archive session';
  $('suspend-session').disabled=!connected||status==='suspended'||status==='closed';
  $('resume-session').hidden=!['suspended','error'].includes(status);
  $('resume-session').disabled=!connected;
  $('show-commands').disabled=!connected||status!=='idle';
  const usage=availableCommands.find(c=>c.name===(active?.provider==='claude'?'usage':'status'));
  $('show-usage').hidden=!['claude','codex'].includes(active?.provider)||!usage;
  $('show-usage').disabled=!connected||status!=='idle';
  const picker=$('model-picker');picker.hidden=!modelConfig?.options?.length;
  if(!picker.hidden){
    const signature=JSON.stringify(modelConfig);
    if(picker.dataset.config!==signature){
      picker.replaceChildren();
      for(const option of modelConfig.options){const node=el('option','',option.name||option.value);node.value=option.value;picker.append(node);}
      picker.value=modelConfig.currentValue;picker.dataset.config=signature;
    }
    picker.disabled=!connected||status!=='idle';
  }
  const busy=['running','waiting','cancelling'].includes(status);
  $('session-state').textContent=status==='waiting'?'Needs your approval':status==='idle'?'Ready':status[0]?.toUpperCase()+status.slice(1);
  $('prompt').disabled=!connected||status!=='idle';$('send').disabled=!connected||status!=='idle';
  $('send').hidden=busy;$('stop').hidden=!busy&&!['starting','resuming','configuring'].includes(status)&&!active?.continuation?.enabled;$('stop').disabled=!connected||status==='cancelling';
  $('prompt').placeholder=status==='starting'?'Starting your agent…':status==='waiting'?'Respond to the permission request above…':busy?'Your agent is working…':status==='error'?'Session unavailable. Use Resume session to try again.':'What would you like to work on?';
}
function scheduleDraw(){if(!drawTimer)drawTimer=setTimeout(()=>{drawTimer=null;draw();},70);}
function fileUrl(path,download=false){return `/api/sessions/${active.id}/file?path=${encodeURIComponent(path)}${download?'&download=true':''}`;}
function localReference(href,base=''){
  if(!href||href.startsWith('#'))return null;
  if(href.startsWith('file://'))return decodeURIComponent(href.slice(7));
  if(/^[a-z][a-z0-9+.-]*:/i.test(href)||href.startsWith('//'))return null;
  const decoded=decodeURIComponent(href.split('#')[0]);
  return decoded.startsWith('/')?decoded:base+decoded;
}
function decorateMarkdown(node,base=''){
  for(const a of node.querySelectorAll('a')){
    const path=localReference(a.getAttribute('href'),base);
    if(path!==null){a.href='#';a.onclick=e=>{e.preventDefault();previewFile(path);};}
    else {a.target='_blank';a.rel='noopener noreferrer';}
  }
  for(const img of node.querySelectorAll('img')){
    const path=localReference(img.getAttribute('src'),base);
    if(path!==null)img.src=fileUrl(path);
    img.loading='lazy';img.alt||='Agent image';
  }
}
function updateMarkdown(message,body){
  if(body.dataset.source===message.text)return;
  body.dataset.source=message.text;
  if(!message.html)body.textContent=message.text;
  if(message.markdownPending)return;
  if(message.markdownTimer)return;
  message.markdownTimer=setTimeout(async()=>{
    message.markdownTimer=null;
    const text=message.text;message.markdownPending=true;
    try{
      const result=await api('/api/markdown',{method:'POST',body:JSON.stringify({text})});
      message.html=result.html;body.innerHTML=result.html;body.classList.add('markdown');decorateMarkdown(body);display.enhance(body,message.id);
    }catch{body.textContent=message.text;}finally{
      message.markdownPending=false;
      if(text!==message.text){delete body.dataset.source;updateMarkdown(message,body);}
    }
  },status==='idle'?0:250);
}
function draw(){
  if(!active)return;
  const area=$('messages');const atBottom=area.scrollHeight-area.scrollTop-area.clientHeight<100;
  const empty=area.querySelector('.session-empty');if(transcript.length)empty?.remove();
  if(!transcript.length&&!empty){
    const wrap=el('div','session-empty');wrap.append(el('div','empty-symbol',icons[active.provider]),el('h3','',status==='starting'?'Preparing your workspace':'Ready when you are'),el('p','',active.provider==='demo'?'Try “hello”, “approval”, or “slow”. This demo uses no API credits.':'Send a message to start working in this project.'));area.append(wrap);
  }
  for(const item of transcript){
    let node=nodes.get(item.id);
    if(!node){
      if(item.type==='message'){
        node=el('article','message '+item.role);node.append(el('div','avatar',item.role==='user'?'Y':icons[active.provider]));
        const content=el('div','message-content');content.append(el('div','message-label',item.role==='user'?'You':labels[active.provider]),el('div','message-body'));node.append(content);
      }else if(item.type==='tool'){
        node=el('details','tool-card');node.append(el('summary'),el('pre'));
      }else if(item.type==='image'){
        node=el('div','message');const image=el('img','agent-image');
        if(/^image\/(png|jpeg|gif|webp)$/.test(item.content.mimeType)&&item.content.data.length<14*1024*1024){image.src=`data:${item.content.mimeType};base64,${item.content.data}`;image.alt='Image from agent';node.append(image);}else node.textContent='Unsupported image format';
      }else node=el('div',item.type==='error'?'error-card':'notice-card',item.text);
      nodes.set(item.id,node);area.append(node);
    }
    if(item.type==='message'){
      const body=node.querySelector('.message-body');
      if(item.role==='user')body.textContent=item.text;else updateMarkdown(item,body);
    }else if(item.type==='tool'){
      const summary=node.querySelector('summary');summary.replaceChildren(el('span','',item.title||'Tool activity'),el('span','tool-state',item.status||'pending'));
      const content=item.content||item.rawOutput||item.rawInput||{};
      node.querySelector('pre').textContent=typeof content==='string'?content:JSON.stringify(content,null,2);
    }
  }
  const pending=$('approval-area');pending.replaceChildren();
  for(const [id,request]of approvals){
    const card=el('div','approval');card.append(el('h4','','Permission requested'),el('p','',request.toolCall?.title||'An agent needs your decision'));
    if(request.toolCall?.rawInput)card.append(el('pre','',JSON.stringify(request.toolCall.rawInput,null,2)));
    for(const option of request.options||[]){
      const button=el('button',option.kind.startsWith('reject')?'reject':'',option.name);button.disabled=!connected;
      button.onclick=async()=>{
        button.disabled=true;
        try{await api(`/api/sessions/${active.id}/approvals/${id}`,{method:'POST',body:JSON.stringify({optionId:option.optionId})});}
        catch(e){toast(e.message);button.disabled=false;}
      };card.append(button);
    }pending.append(card);
  }
  if(restoreScroll!==null){area.scrollTop=restoreScroll;restoreScroll=null;}else if(atBottom)area.scrollTop=area.scrollHeight;
}
async function previewFile(path){
  const sid=active?.id;if(!sid)return;
  $('file-open-workspace').onclick=()=>{$('file-dialog').close();if(active?.id===sid)workspace?.openSessionFile(active,path);};
  $('file-title').textContent=path;$('file-content').replaceChildren(el('p','','Loading…'));
  const download=$('file-download');download.href=fileUrl(path,true);download.download=basename(path);
  if(!$('file-dialog').open)$('file-dialog').showModal();
  try{
    const response=await fetch(fileUrl(path));if(!response.ok)throw new Error(await response.text());
    if(active?.id!==sid)return;
    const content=$('file-content');content.replaceChildren();const type=response.headers.get('content-type');
    if(type?.startsWith('image/')){
      const image=el('img','file-image');image.src=fileUrl(path);image.alt=basename(path);content.append(image);
    }else if(type?.startsWith('text/')){
      const text=await response.text();
      if(/\.md$/i.test(path)){
        const {html}=await api('/api/markdown',{method:'POST',body:JSON.stringify({text:text.slice(0,100000)})});content.innerHTML=html;
        const base=path.includes('/')?path.slice(0,path.lastIndexOf('/')+1):'';decorateMarkdown(content,base);display.enhance(content,'file:'+path);
      }else{content.append(el('pre','',text));display.enhance(content,'file:'+path,{language:/\.([\w+-]+)$/.exec(basename(path))?.[1]||''});}
    }else content.append(el('p','','Preview is not available for this format. Use Download to save the file.'));
  }catch(e){$('file-content').textContent=e.message;}
}
$('new-session').onclick=()=>openCreate();$('welcome-new').onclick=()=>openCreate();
$('view-hosts').onclick=()=>setSidebarView('hosts');$('view-projects').onclick=()=>setSidebarView('projects');
$('new-project').onclick=()=>openProjectDialog();$('create-project-new').onclick=()=>openProjectDialog();
$('dismiss-project').onclick=()=>{if(!savingProject)$('project-dialog').close();};
$('project-dialog').addEventListener('cancel',event=>{if(savingProject)event.preventDefault();});
$('create-project').onchange=()=>{const remembered=readStorage(localStorage,'agentworks-project-locations',{})[$('create-project').value||'ungrouped'];if(remembered)$('directory').value=remembered;worktrees.projectChanged();};
$('project-form').onsubmit=async event=>{
  event.preventDefault();if(savingProject)return;savingProject=true;
  for(const id of ['save-project','project-name','project-default-mode','project-require-worktree','delete-project','dismiss-project'])$(id).disabled=true;
  $('project-error').textContent='';
  try{
    const policy=policyEditor.value();
    const project=await api(editingProject?`/api/projects/${editingProject}`:'/api/projects',{method:editingProject?'PATCH':'POST',body:JSON.stringify({name:$('project-name').value})});
    editingProject=project.id;$('delete-project').hidden=false;
    try{await api(`/api/projects/${project.id}/workspace-policy`,{method:'PUT',body:JSON.stringify(policy)});}
    catch(error){throw new Error(`Project name saved. Workspace policy was not saved: ${error.message}. Retry Save project.`);}
    await refreshSessions();if($('create-dialog').open){$('create-project').value=project.id;worktrees.projectChanged();}$('project-dialog').close();
  }catch(error){$('project-error').textContent=error.message;}finally{savingProject=false;for(const id of ['save-project','project-name','project-default-mode','project-require-worktree','delete-project','dismiss-project'])$(id).disabled=false;}
};
$('delete-project').onclick=async()=>{
  if(!editingProject||!confirm('Delete this project group? Its sessions will move to Ungrouped and keep running.'))return;
  try{await api(`/api/projects/${editingProject}`,{method:'DELETE'});$('project-dialog').close();await refreshSessions();}catch(error){$('project-error').textContent=error.message;}
};
$('session-project').onchange=async event=>{
  try{const updated=await api(`/api/sessions/${active.id}/project`,{method:'POST',body:JSON.stringify({projectId:event.target.value||null})});Object.assign(active,updated);workspace?.sessionSelected(active);await refreshSessions();}
  catch(error){toast(error.message);updateProjectControls();}
};
$('dismiss-create').onclick=()=>$('create-dialog').close();$('dismiss-file').onclick=()=>$('file-dialog').close();
$('menu').onclick=()=>document.body.classList.toggle('menu-open');$('scrim').onclick=()=>document.body.classList.remove('menu-open');
$('provider').onchange=()=>{providerNote();resetCreateModels();};
$('directory').onchange=resetCreateModels;
$('load-models').onclick=loadCreateModels;
$('show-commands').onclick=()=>{commandsOpen=!commandsOpen;renderCommands();};
$('show-usage').onclick=async()=>{
  if(!active||status!=='idle')return;
  $('show-usage').disabled=true;
  try{await api(`/api/sessions/${active.id}/prompt`,{method:'POST',body:JSON.stringify({text:active.provider==='claude'?'/usage':'/status',requestId:uuid()})});}
  catch(e){toast(e.message);}finally{updateControls();}
};
$('model-picker').onchange=async event=>{
  try{await api(`/api/sessions/${active.id}/model`,{method:'POST',body:JSON.stringify({model:event.target.value})});}
  catch(error){toast(error.message);event.target.value=modelConfig.currentValue;}
};
$('create-form').onsubmit=async event=>{
  event.preventDefault();if(creating||worktrees.blocked())return;creating=true;providerNote();$('create-error').textContent='';
  try{
    const payload={resumePolicy:{enabled:$('create-resume-policy').checked},provider:$('provider').value,...worktrees.submission(),title:$('session-title').value,projectId:$('create-project').value||null,hostId:$('create-host').value,model:$('create-model').value,agentAccess:{communicate:$('create-agent-access').value!=='off',...Object.fromEntries(['startSessions','promptSessions','cancelSessions','runAutomations'].map(key=>[key,$('create-agent-access').value==='coordinator']))}};
    const signature=JSON.stringify(payload);
    if(!pendingCreate||pendingCreate.signature!==signature)pendingCreate={signature,requestId:uuid()};
    sessionStorage.setItem('agentworks-pending-create',JSON.stringify(pendingCreate));
    const created=await api('/api/sessions',{method:'POST',body:JSON.stringify({...payload,requestId:pendingCreate.requestId})});
    pendingCreate=null;sessionStorage.removeItem('agentworks-pending-create');
    const locations=readStorage(localStorage,'agentworks-project-locations',{});locations[created.projectId||'ungrouped']=created.cwd;localStorage.setItem('agentworks-project-locations',JSON.stringify(locations));
    sessions.unshift(created);$('create-dialog').close();selectSession(created);
  }catch(e){$('create-error').textContent=e.message;}finally{creating=false;providerNote();}
};
$('composer').onsubmit=async event=>{
  event.preventDefault();const text=$('prompt').value;if(!active||status!=='idle'||!text.trim())return;
  if(!pendingSend||pendingSend.text!==text||pendingSend.sid!==active.id)pendingSend={text,sid:active.id,requestId:uuid()};
  $('send').disabled=true;
  const submission={...pendingSend};
  try{await api(`/api/sessions/${submission.sid}/prompt`,{method:'POST',body:JSON.stringify(submission)});if(active?.id===submission.sid&&$('prompt').value===submission.text)$('prompt').value='';if(drafts[submission.sid]===submission.text)delete drafts[submission.sid];sessionStorage.setItem('agentworks-drafts',JSON.stringify(drafts));if(pendingSend?.requestId===submission.requestId)pendingSend=null;}
  catch(e){toast(e.message);}finally{updateControls();}
};
$('prompt').oninput=()=>{commandsOpen=/^[/\$]\S*$/.test($('prompt').value);renderCommands();if(active){drafts[active.id]=$('prompt').value;sessionStorage.setItem('agentworks-drafts',JSON.stringify(drafts));}};
$('prompt').onkeydown=event=>{if(event.key==='Enter'&&!event.shiftKey&&!event.isComposing){event.preventDefault();$('composer').requestSubmit();}};
$('resume-session').onclick=()=>{if(active)resumeSession(active.id);};
$('stop').onclick=async()=>{try{await api(`/api/sessions/${active.id}/cancel`,{method:'POST',body:'{}'});}catch(e){toast(e.message);}};
$('close-session').onclick=async()=>{
  if(!active)return;
  if(!confirm('Permanently delete this session and its saved transcript? Its harness will stop. Worktree files and branches remain. To keep history, choose Suspend harness instead. This cannot be undone.'))return;
  const id=active.id;
  try{await api(`/api/sessions/${id}`,{method:'DELETE'});generation++;socket?.close();clearTimeout(retryTimer);active=null;localStorage.removeItem('agentworks-session');$('conversation').hidden=true;$('welcome').hidden=false;$('header-title').textContent='Overview';await refreshSessions();}catch(e){toast(e.message);}
};
function applyRoots(){
  $('project-roots').replaceChildren();
  for(const root of config.roots){const option=el('option');option.value=root;$('project-roots').append(option);}
  $('roots-note').textContent='Allowed roots: '+config.roots.join(', ');workspace?.rootsChanged();
}
function rootValues(){return [...$('settings-roots').querySelectorAll('input')].map(input=>input.value.trim());}
function addRoot(value=''){
  const row=el('div','root-item'),input=el('input');input.value=value;input.placeholder='/home/you/projects';input.setAttribute('aria-label','Allowed root directory');input.autocomplete='off';input.spellcheck=false;input.required=true;
  const remove=el('button','icon-button','×');remove.type='button';remove.setAttribute('aria-label','Remove root directory');remove.onclick=()=>{row.remove();if(!$('settings-roots').children.length)addRoot();};
  row.append(input,remove);$('settings-roots').append(row);return input;
}
$('add-root').onclick=()=>addRoot().focus();
$('open-settings').onclick=async()=>{
  try{
    const settings=await api('/api/settings');$('settings-roots').replaceChildren();for(const root of settings.roots)addRoot(root);$('settings-error').textContent='';$('settings-saved').textContent='';
    const fields=$('default-model-fields');fields.replaceChildren();
    for(const provider of config.providers.filter(p=>p.id!=='demo')){
      const id='default-model-'+provider.id,label=el('label','',provider.name+' default model');label.htmlFor=id;
      const input=el('input');input.id=id;input.dataset.provider=provider.id;input.value=settings.defaultModels[provider.id]||'';input.placeholder='Harness default';input.maxLength=256;
      const list=el('datalist');list.id=id+'-options';input.setAttribute('list',list.id);
      const load=el('button','text-button','Load models');load.type='button';
      load.onclick=async()=>{
        load.disabled=true;load.textContent='Loading…';$('settings-error').textContent='';
        try{
          const cwd=rootValues().find(Boolean)||config.roots[0];
          const result=await api('/api/models',{method:'POST',body:JSON.stringify({provider:provider.id,cwd})});
          list.replaceChildren();for(const model of result.options||[]){const option=el('option');option.value=model.value;option.label=model.name||model.value;list.append(option);}
          if(!result.options?.length)$('settings-error').textContent='This harness did not advertise selectable models.';
        }catch(e){$('settings-error').textContent=e.message;}finally{load.disabled=false;load.textContent='Load models';}
      };
      const row=el('div','model-field');row.append(input,load);fields.append(label,row,list);
    }
    $('settings-dialog').showModal();
  }catch(e){toast(e.message);}
};
$('dismiss-settings').onclick=()=>$('settings-dialog').close();
$('settings-form').onsubmit=async event=>{
  event.preventDefault();$('save-settings').disabled=true;$('settings-error').textContent='';
  try{
    const defaultModels={};for(const input of $('default-model-fields').querySelectorAll('input')){if(input.value.trim())defaultModels[input.dataset.provider]=input.value.trim();}
    const settings=await api('/api/settings',{method:'PUT',body:JSON.stringify({roots:rootValues().filter(Boolean),defaultModels})});
    config.roots=settings.roots;config.defaultModels=settings.defaultModels;applyRoots();$('settings-dialog').close();toast('Settings saved. They apply to new sessions.','success');
  }catch(e){$('settings-error').textContent='Host settings were not saved: '+e.message+' Display preferences are stored separately in this browser.';}finally{$('save-settings').disabled=false;}
};
document.addEventListener('keydown',event=>{if(event.key==='n'&&!event.ctrlKey&&!event.metaKey&&!event.altKey&&!['INPUT','TEXTAREA','SELECT'].includes(document.activeElement?.tagName)&&!document.querySelector('dialog[open]')){event.preventDefault();openCreate();}});
setupDiff({api,getActive:()=>active});
workspace=createWorkspace({api,toast,labels,uuid,getConfig:()=>config,getActive:()=>active,selectSession,openCreate,refreshSessions,enhanceBlocks:display.enhance,openSettings:()=>$('open-settings').click(),
  overview(){ $('conversation').hidden=true;$('welcome').hidden=false;localStorage.removeItem('agentworks-session'); },
  appendDraft(text){if(!active)return;const current=$('prompt').value;$('prompt').value=current?current+'\n\n'+text:text;drafts[active.id]=$('prompt').value;sessionStorage.setItem('agentworks-drafts',JSON.stringify(drafts));},
});
async function init(){
  config=await api('/api/status');
  $('host-address').textContent=location.host+' · Linux prototype';
  for(const p of config.providers){const option=el('option','',p.name+(p.authMode==='api_key'&&!p.keyPresent?' · API key needed':''));option.value=p.id;$('provider').append(option);}
  applyRoots();
  await refreshSessions();
  const saved=sessions.find(s=>s.id===localStorage.getItem('agentworks-session'));if(saved)selectSession(saved);workspace.restoreView();
  setInterval(()=>refreshSessions().catch(()=>{}),5000);
}
init().catch(error=>toast('Cannot reach the local server: '+error.message));

setupCommunication({api,getActive:()=>active,getSessions:()=>sessions,selectSession,toast,uuid});

setupGitContext({api,getActive:()=>active});

// A chosen harness command only reaches the composer; nothing here submits a turn.
function insertCommand(text) {
  if(!active)return;
  const current=$('prompt').value;
  $('prompt').value=/^\s*[/\$]\S*$/.test(current)||!current.trim()?text+' ':current.replace(/\s+$/,'')+' '+text+' ';
  $('prompt').dispatchEvent(new Event('input'));
  $('prompt').focus();
}
function usageCommand(){return availableCommands.find(c=>c.name===(active?.provider==='claude'?'usage':'status'));}
setupPalettes({
  $,labels,selectSession,
  fallbackFocus:()=>active?$('prompt'):null,
  state:()=>({active,status,connected,sessions,projects,hosts,commands:availableCommands,
    usageAvailable:!!active&&['claude','codex'].includes(active.provider)&&!!usageCommand(),
    views:{files:()=>$('nav-files').click(),automations:()=>$('nav-automations').click()}}),
  handlers:{
    newSession:()=>openCreate(),
    importSession:()=>openImport(),
    newProject:()=>openProjectDialog(),
    settings:()=>$('open-settings').click(),
    agents:()=>{if(active)$('session-agents').click();},
    workspaceInfo:()=>{if(active)$('session-workspace').click();},
    diff:()=>{if(active)$('session-diff').click();},
    lifecycle:()=>{if(active){$('lifecycle-menu').open=true;$('lifecycle-menu').querySelector('button').focus();}},
    resume:()=>{if(active&&connected&&['suspended','error'].includes(status))resumeSession(active.id);},
    stop:()=>{if(active&&connected&&['running','waiting'].includes(status))$('stop').click();},
    usage:()=>{if(active&&connected&&status==='idle')$('show-usage').click();},
    deleteSession:()=>{if(active)$('close-session').click();},
    insertCommand,
  },
});

async function refreshContinuation(){
  const id=active?.id;if(!id)return;
  try{const snapshot=await api(`/api/sessions/${id}`);if(active?.id===id){active.continuation=snapshot.session.continuation;renderContinuation();updateControls();}}catch(e){toast(e.message);}
}
function renderContinuation(){
  const p=active?.continuation||{};
  const boundKey=active?.id+':'+(p.authorization||0);
  if($('continuation-attempts').dataset.policy!==boundKey){
    $('continuation-attempts').dataset.policy=boundKey;$('continuation-attempts').value=p.maxAttempts||3;
    const d=new Date(p.deadline||Date.now()+86400000);$('continuation-deadline').value=new Date(d.getTime()-d.getTimezoneOffset()*60000).toISOString().slice(0,16);
  }
  $('continuation-enabled').checked=!!p.enabled;
  $('continuation-label').textContent=p.enabled?'On':'Off';
  const due=p.dueAt?new Date(p.dueAt):null;
  $('continuation-status').textContent=[p.limited?'Paused by usage limit':'',p.reason,due?`Resumes ${due.toLocaleString()} · ${Math.max(0,Math.ceil((p.dueAt-Date.now())/1000))}s remaining`:p.limited?'Reset unknown or timer cancelled — choose a time or resume deliberately':'',`Policy: ${p.origin||'default off'}${p.parent?' · parent '+p.parent:''}`,p.enabled?`${p.attempts}/${p.maxAttempts} attempts · deadline ${new Date(p.deadline).toLocaleString()}`:'',p.source,active?.provider==='opencode'?'OpenCode automatic reset detection unavailable; use manual session controls':'',p.observedAt?'Data observed '+new Date(p.observedAt).toLocaleString():'No provider reset data'].filter(Boolean).join(' · ');
  $('continuation-schedule').disabled=!p.enabled||!p.limited;
  $('continuation-now').disabled=(!p.limited&&readStorage(sessionStorage,'agentworks-manual-continuation',null)?.sid!==active?.id)||!!$('continuation-now').dataset.busy;
}
async function setContinuation(payload){if(!active)return;try{await api(`/api/sessions/${active.id}/continuation`,{method:'PUT',body:JSON.stringify(payload)});await refreshContinuation();}catch(e){toast(e.message);}}
$('continuation-save-bounds').onclick=()=>{const deadline=new Date($('continuation-deadline').value).getTime(),maxAttempts=Number($('continuation-attempts').value);if(!Number.isFinite(deadline)||!Number.isInteger(maxAttempts)){toast('Choose a valid deadline and attempt count');return;}setContinuation({enabled:true,deadline,maxAttempts});};
$('continuation-enabled').onchange=()=>setContinuation({enabled:$('continuation-enabled').checked});
$('continuation-cancel').onclick=()=>setContinuation({enabled:false});
$('continuation-schedule').onclick=()=>{const due=new Date($('continuation-time').value).getTime();if(!Number.isFinite(due)){toast('Choose a valid local time');return;}setContinuation({enabled:true,manualDueAt:due});};
$('continuation-now').onclick=async()=>{
  if(!active||$('continuation-now').dataset.busy)return;
  const id=active.id,button=$('continuation-now');button.dataset.busy='yes';button.disabled=true;
  let pending=readStorage(sessionStorage,'agentworks-manual-continuation',null);
  if(pending?.sid!==id)pending={sid:id,requestId:uuid(),started:false};
  sessionStorage.setItem('agentworks-manual-continuation',JSON.stringify(pending));
  try{
    if(!pending.started){
      await api(`/api/sessions/${id}/cancel`,{method:'POST',body:'{}'});
      await api(`/api/sessions/${id}/resume`,{method:'POST',body:'{}'});
      for(let i=0;i<300;i++){
        const current=await api(`/api/sessions/${id}`);
        if(current.session.status==='idle')break;
        if(current.session.status==='error')throw new Error('Native resume failed; inspect session error');
        await new Promise(r=>setTimeout(r,250));if(i===299)throw new Error('Native session is not ready');
      }
      pending.started=true;sessionStorage.setItem('agentworks-manual-continuation',JSON.stringify(pending));
    }
    await api(`/api/sessions/${id}/prompt`,{method:'POST',body:JSON.stringify({text:'Inspect current task and tool results, then continue the unfinished assigned task within its saved scope and stop instructions. Do not replay prior commands. If complete, report completion.',requestId:pending.requestId})});
    sessionStorage.removeItem('agentworks-manual-continuation');
  }catch(e){toast(e.message);}finally{delete button.dataset.busy;renderContinuation();}
};
setInterval(()=>{if(active?.continuation?.dueAt)renderContinuation();},1000);

$('session-filter').value=readStorage(localStorage,'agentworks-session-filter','active');
$('session-filter').onchange=()=>{localStorage.setItem('agentworks-session-filter',JSON.stringify($('session-filter').value));sidebar();};
$('rename-session').onclick=()=>{$('rename-title').value=active.title;$('rename-error').textContent='';$('rename-dialog').showModal();$('rename-title').focus();};
$('rename-dismiss').onclick=()=>$('rename-dialog').close();
$('rename-form').onsubmit=async event=>{
  event.preventDefault();const id=active?.id;if(!id)return;
  try{const updated=await api(`/api/sessions/${id}`,{method:'PATCH',body:JSON.stringify({title:$('rename-title').value})});if(active?.id===id){Object.assign(active,updated);$('header-title').textContent=updated.title;}await refreshSessions();$('rename-dialog').close();}catch(e){$('rename-error').textContent=e.message;}
};
$('archive-session').onclick=async()=>{
  const id=active?.id;if(!id)return;
  try{const updated=await api(`/api/sessions/${id}`,{method:'PATCH',body:JSON.stringify({archived:!active.archived})});if(active?.id===id)Object.assign(active,updated);await refreshSessions();updateControls();}catch(e){toast(e.message);}
};
$('suspend-session').onclick=async()=>{
  const id=active?.id;if(!id)return;
  try{const updated=await api(`/api/sessions/${id}/suspend`,{method:'POST',body:'{}'});if(active?.id===id){Object.assign(active,updated);status=updated.status;updateControls();}await refreshSessions();}catch(e){toast(e.message);}
};

$('dismiss-lifecycle').onclick=()=>{$('lifecycle-menu').open=false;$('lifecycle-menu').querySelector('summary').focus();};
$('lifecycle-menu').addEventListener('keydown',event=>{if(event.key==='Escape'){$('dismiss-lifecycle').click();event.stopPropagation();}});

// Polling never submits or replays a restart request. Identity is server-owned and durable.
const serviceSection=document.createElement('fieldset');serviceSection.id='service-controls';
serviceSection.innerHTML=`<legend>Service · all projects</legend><p id="service-status" role="status" aria-live="polite">Loading service status…</p><label for="service-mode">Restart behavior</label><select id="service-mode"><option value="wait">Wait until all work is idle</option><option value="interrupt">Interrupt ALL active sessions</option></select><label for="service-expiry">Drain expiry (seconds, 5–900)</label><input id="service-expiry" type="number" min="5" max="900" value="300"><label for="service-release">Prepared release (blank keeps current executable)</label><input id="service-release" maxlength="128" placeholder="Immutable release basename"><button type="button" id="service-restart">Restart service</button><button type="button" id="service-cancel" hidden>Cancel pending restart</button><p id="service-error" class="error-text" role="alert"></p>`;
$('display-preferences').after(serviceSection);
let servicePending=null,servicePolling=false;
async function refreshService(){
 if(servicePolling||!$('settings-dialog').open)return;servicePolling=true;
 try{
  const s=await api('/api/service/restart');const a=s.activity;servicePending=s.requests.find(r=>['draining','dispatching'].includes(r.phase)||['pending','opening'].includes(r.handoffPhase));
  const latest=s.requests[0];const remaining=servicePending?Math.max(0,Math.ceil((servicePending.expiresAt-Date.now())/1000)):0;
  $('service-status').textContent=`Instance ${s.instanceId.slice(0,8)} · ${s.restartSupported?'Supervised restart available':'Restart unavailable: external supervisor required'}. ${a.activeTurns} active turns, ${a.approvals} approvals, ${a.workspacePreparations} workspace preparations, ${a.startingHarnesses} harnesses starting.`+(latest?` Request ${latest.request.requestId}: ${latest.phase} · ${latest.detail}${latest.handoffPhase?' · handoff '+latest.handoffPhase:''}.`:'')+(servicePending?` Drain expires in ${remaining}s.`:'')+(s.managerError?' '+s.managerError:'')+(s.supervisorOutcome?.phase==='failed'?` Supervisor failed: ${s.supervisorOutcome.detail}`:'');
  $('service-restart').disabled=!s.restartSupported||!!servicePending;$('service-cancel').hidden=!servicePending||servicePending.phase==='dispatching';
 }catch(e){$('service-status').textContent='Service disconnected. Waiting to reconnect and read the durable outcome…';}finally{servicePolling=false;}
}
$('service-restart').onclick=async()=>{
 const interrupt=$('service-mode').value==='interrupt';
 if(!confirm(interrupt?'GLOBAL service restart will interrupt active turns and approvals in ALL projects. Restart now?':'GLOBAL service restart will block new work in ALL projects, wait until idle, then disconnect all sessions. Continue?'))return;
 const requestId=uuid();$('service-restart').disabled=true;$('service-error').textContent='';
 try{await api('/api/service/restart',{method:'POST',body:JSON.stringify({requestId,mode:$('service-mode').value,expiresInSeconds:Number($('service-expiry').value),release:$('service-release').value.trim()||null})});}
 catch(e){$('service-error').textContent=e.message+' Check durable status before requesting again.';}await refreshService();
};
$('service-cancel').onclick=async()=>{if(!servicePending)return;try{await api('/api/service/restart/cancel',{method:'POST',body:JSON.stringify({actor:servicePending.actor,requestId:servicePending.request.requestId})});await refreshService();}catch(e){$('service-error').textContent=e.message;}};
setInterval(refreshService,1000);$('open-settings').addEventListener('click',()=>setTimeout(refreshService,150));
