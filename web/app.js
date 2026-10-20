import {appUrl, socketUrl, localState as localStorage, sessionState as sessionStorage} from './hosting.js';
import {setupInboxDelivery} from './inbox-delivery.js';
import {setupDiff} from './diff.js';
import {setupWorktreeControls,setupPolicyEditor} from './worktree-controls.js';
import {setupGitContext} from './git-context.js';
import {setupWorktreeOwnership} from './worktree-ownership.js';
import {setupControllers} from './controllers.js';
import {createWorkspace} from './workspace.js';
import {setupCommunication} from './communication.js';
import {setupPalettes} from './palettes.js';
import {setupDisplayPreferences} from './display-preferences.js';
import {setupWorkLogs,observeTool} from './work-logs.js';
import {setupUsageStatus} from './usage-status.js';
import {setupFlyout} from './flyout.js';
import {setupResize} from './resize-handle.js';
import {setupSidePanel} from './side-panel.js';
import {setupContextMenu} from './context-menu.js';
import {setupTimeoutControls} from './timeout-controls.js';
import {setupInputQueue,BUSY} from './input-queue.js';
import {setupNotifications} from './notifications.js';
import {setupQuestions} from './questions.js';
import {setupMentions} from './mentions.js';
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
// U-07: per-session composer usage statusline; only this session's own events feed it.
const usageStatus=setupUsageStatus({root:$('usage-status'),getProvider:()=>active?.provider,chip:$('composer-info')});
const composerMenu=setupFlyout({menu:$('composer-menu'),triggers:[$('composer-more'),$('composer-info')]});
const sessionMenu=setupFlyout({menu:$('session-menu'),triggers:[$('session-more')]});
setupResize({handle:$('sidebar-resize'),target:document.querySelector('.layout'),panel:$('sidebar'),variable:'--sidebar-width',key:'agentworks-sidebar-width',min:200,max:560,initial:228});
setupResize({handle:$('side-panel-resize'),target:$('side-panel'),panel:$('side-panel'),variable:'--side-panel-width',key:'agentworks-side-panel-width',min:300,max:960,initial:420,edge:'left'});
let sidePanel=null;
setInterval(()=>usageStatus.render(),60000);
let timeoutControls=null;
let transcript=[], tools=new Map(), approvals=new Map(), assistant=null, status='starting';
// L-05 hook: activity indicator and explicit queued inputs (server-owned state).
const questions=setupQuestions({api,uuid:()=>uuid(),storage:localStorage,getActive:()=>active,isConnected:()=>connected,agentName:()=>labels[active?.provider]||'the agent'});
const inputQueue=setupInputQueue({$,api,uuid:()=>uuid(),getActive:()=>active,getStatus:()=>status,toolActive:()=>[...tools.values()].some(t=>['pending','in_progress'].includes(t.status))});
let permissionModes=null;
const mentions=setupMentions({prompt:$('prompt'),list:$('mention-list'),getActive:()=>active,getSessions:()=>sessions});
let replaying=false,workLogs=null,turnEnds=new Map();
let modelConfig=null, availableCommands=[], commandsOpen=false;
let discoveryGeneration=0;
let externalSessions=[], externalSelection=null, externalCursor=null, externalGeneration=0, externalLoading=false, externalImporting=false;
let externalFetch=Promise.resolve();
let drawTimer=null, retryTimer=null, reconnectDelay=500, creating=false;
const nodes=new Map();
let pendingCreate=readStorage(sessionStorage,'agentworks-pending-create',null);
// L-08: set only while the new-session dialog is pre-filled from an existing session.
let forkSource=null,forkGeneration=0,modelPicked=false;
const worktrees=setupWorktreeControls({$,api,onChange:()=>{if(config)providerNote();}});
const policyEditor=setupPolicyEditor({$,api});
const display=setupDisplayPreferences();display.mount($('display-preferences'));
workLogs=setupWorkLogs({el});workLogs.mount($('display-preferences'),()=>scheduleDraw());
// U-02: policy lives on the server; the browser only requests permission and shows.
const notifications=setupNotifications({getSessions:()=>sessions,getActive:()=>active,selectSession,toast});

async function api(path, options={}) {
  const response=await fetch(appUrl(path),{...options,headers:{'Content-Type':'application/json',...options.headers}});
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
const communicationLabels={off:'Off',participant:'Participant',coordinator:'Coordinator'};
function updateCommunicationDefault(){
  const project=projects.find(p=>p.id===$('create-project').value);
  const role=project?.defaultCommunicationRole||config.defaultCommunicationRole||'participant';
  $('create-agent-access').querySelector('option[value=""]').textContent=`Use ${project?.defaultCommunicationRole?'project':'global'} default · ${communicationLabels[role]}`;
  $('create-communication-note').textContent='Participant exchanges project messages. Coordinator can also start sessions and turns, cancel turns, and run project automations. Change session access later in Agents & inbox.';
}
function openCreate(context={}) {
  if(!config)return;
  updateProjectControls();
  forkSource=context.fork||null;const generation=++forkGeneration;modelPicked=false;
  const workspaceContext=workspace?.context();
  const projectId=Object.hasOwn(context,'projectId')?context.projectId:workspaceContext?.projectId??active?.projectId??null;
  $('create-project').value=projectId||'';
  const locations=readStorage(localStorage,'agentworks-project-locations',{});
  $('create-resume-policy').checked=false;
  $('create-agent-access').value='';updateCommunicationDefault();$('create-error').textContent='';$('directory').value=context.cwd||workspaceContext?.cwd||locations[projectId||'ungrouped']||active?.cwd||config.roots[0];$('session-title').value=context.title||'';
  if(context.provider&&$('provider').querySelector(`option[value="${context.provider}"]`))$('provider').value=context.provider;
  $('fork-boundary').replaceChildren(new Option('Latest completed turn at submission',''));$('fork-context').value='auto';$('fork-summary').value='';
  renderForkHeader();resetCreateModels();providerNote();
  // Forking suggests its own worktree so the new session starts from this one's
  // position; the base revision is filled in from Git once the policy has loaded.
  worktrees.projectChanged(context.workspaceMode);
  $('create-dialog').showModal();
  return generation;
}
function renderForkHeader(){
  const fork=forkSource;
  $('create-fork-note').hidden=!fork;$('fork-options').hidden=!fork;
  $('create-eyebrow').textContent=fork?'BRANCH OFF':'MAKE SOME SPACE';
  $('create-heading').textContent=fork?'Fork session':'New session';
  $('create-submit').textContent=fork?'Fork session ↗':'Create session ↗';
  $('create-import').hidden=!!fork;
  if(fork)$('create-fork-note').textContent=`Carrying over ${fork.turns?`${fork.turns} finished turn${fork.turns===1?'':'s'}`:'the recorded conversation'} from “${fork.title}”. A supported native fork is preferred; otherwise its transcript is handed over with your first message, not as hidden harness state.`;
}
// A model only carries over to the same harness. Applied once the parent's own
// snapshot answers, so it does not depend on the event stream being connected.
function applyForkModel(model){
  if(!model||forkSource?.model===model)return;
  forkSource.model=model;
  // Never replace a model the human has already chosen in this dialog.
  if(!modelPicked)resetCreateModels();
}
// L-08: the session a fork is taken from. Cleared by any non-fork open.
async function openFork(parent=active){
  if(!parent||!config)return;
  $('lifecycle-menu').open=false;
  const generation=openCreate({fork:{id:parent.id,title:parent.title,provider:parent.provider,cwd:parent.cwd,model:(active?.id===parent.id?modelConfig?.currentValue:null)||parent.model||null,turns:0},
    projectId:parent.projectId??null,cwd:parent.cwd,title:`${parent.title} (fork)`,provider:parent.provider,workspaceMode:'newWorktree'});
  const id=parent.id;
  try{
    // The parent's committed position becomes the fork's default base revision, but
    // only once the suggested worktree mode has actually been applied.
    const [,workspace,parentState]=await Promise.all([worktrees.settled(),api(`/api/sessions/${id}/workspace`).catch(()=>null),api(`/api/sessions/${id}`).catch(()=>null)]);
    if(generation!==forkGeneration||forkSource?.id!==id||!$('create-dialog').open)return;
    if(workspace?.commit&&$('workspace-mode').value==='newWorktree'&&!$('worktree-base').value)$('worktree-base').value=workspace.commit;
    applyForkModel(parentState?.models?.currentValue);
    const completed=(parentState?.events||[]).filter(e=>e.kind==='turn_end'&&e.data?.stopReason!=='cancelled'&&!e.data?.timedOut);
    forkSource.turns=completed.length;
    for(const [i,e] of completed.entries())$('fork-boundary').append(new Option(`Completed turn ${i+1} · event ${e.seq}`,String(e.seq)));
    renderForkHeader();
  }catch(e){toast(e.message);}
}
function resetCreateModels(){
  discoveryGeneration++;
  const select=$('create-model');select.replaceChildren();
  // A forked session keeps the parent's model when it keeps the parent's harness.
  const carried=forkSource?.model&&forkSource.provider===$('provider').value?forkSource.model:null;
  const defaultId=carried||config.defaultModels?.[$('provider').value];
  const option=el('option','',carried?`From “${forkSource.title}” · ${carried}`:defaultId?`Saved default · ${defaultId}`:'Harness default');option.value=defaultId||'';select.append(option);
  $('load-models').disabled=false;$('model-note').textContent='Load available models from the agent without sending a prompt.';
}
async function loadCreateModels(){
  const gen=++discoveryGeneration,provider=$('provider').value,cwd=$('directory').value;
  modelPicked=true;
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
  $('import-directory').value='';$('import-search').value='';resetExternal();$('import-dialog').showModal();browseExternal();
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
    // Let an in-flight harness finish before starting the latest selection.
    // Rapid switches must not exhaust the server's discovery slots.
    const previous=externalFetch;let release;
    externalFetch=new Promise(resolve=>{release=resolve;});
    let result;
    try{
      await previous;if(gen!==externalGeneration)return;
      result=await api('/api/external-sessions',{method:'POST',body:JSON.stringify({provider,cwd:cwd||null,cursor:more?externalCursor:null,hostProfile})});
    }finally{release();}
    if(gen!==externalGeneration)return;
    const seen=new Set(externalSessions.map(s=>s.sessionId));for(const session of result.sessions||[])if(!seen.has(session.sessionId)){externalSessions.push(session);seen.add(session.sessionId);}
    externalCursor=result.nextCursor||null;
  }catch(e){if(gen===externalGeneration)$('import-error').textContent=e.message;}
  finally{if(gen===externalGeneration){externalLoading=false;renderExternal();externalControls();}}
}
$('import-session').onclick=openImport;$('create-import').onclick=openImport;
$('dismiss-import').onclick=()=>{$('import-dialog').close();externalGeneration++;};
function changeExternalAgent(){
  $('import-search').value='';resetExternal();browseExternal();
}
$('import-provider').onchange=changeExternalAgent;$('import-source').onchange=changeExternalAgent;$('import-directory').oninput=resetExternal;
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
  for(const id of ['create-project','session-project','import-project']){
    const select=$(id), current=id==='session-project'?active?.projectId:select.value;
    const ordered=projects.filter(p=>!p.archived||p.id===current).sort((a,b)=>a.name.localeCompare(b.name)), signature=JSON.stringify(ordered);
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
function treeGroup(key,label,items,{host=false,projectId=null,level=0,archived=false}={}){
  const group=el('details','tree-group level-'+level);group.dataset.group=key;
  const stateKey=sidebarView+':'+key;group.open=!collapsedGroups.has(stateKey);
  const heading=el('summary','tree-heading');if(!host&&projectId){heading.dataset.projectId=projectId;heading.setAttribute('aria-haspopup','menu');}heading.append(el('span','tree-chevron','›'));
  heading.append(el('span',host?'tree-icon host':'tree-icon',host?'▤':'◇'),el('span','tree-label',label),badges(items));
  if(archived)heading.append(el('span','group-total','Archived'));
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
  const copy=el('span','session-copy');copy.append(el('strong','',s.title),el('small','',`${s.archived?'Archived · ':''}${s.automation?'Automation · ':''}${labels[s.provider]}${s.model?' · '+s.model:''}`));
  button.append(copy,el('span','status-dot '+s.status));button.onclick=()=>selectSession(s);
  if(active?.id===s.id)button.setAttribute('aria-current','true');return button;
}
// Keep the session browser's model label in step with live model changes.
function syncModelLabel(){
  const row=active&&sessions.find(s=>s.id===active.id),value=modelConfig?.currentValue;
  if(!row||!value)return;
  const model=modelConfig.options?.find(o=>o.value===value)?.name||value;
  if(row.model!==model){row.model=model;if(!replaying)sidebar();}
}
function sidebar(){
  const filter=$('session-filter').value;
  const visibleSessions=sessions.filter(s=>filter==='all'||!!s.archived===(filter==='archived'));
  // Archived projects appear with archived sessions; active projects holding archived sessions do too.
  const visibleProjects=projects.filter(p=>filter==='all'||(filter==='archived'?p.archived||visibleSessions.some(s=>s.projectId===p.id):!p.archived));
  $('session-count').textContent=visibleSessions.length;
  for(const view of ['hosts','projects'])$('view-'+view).setAttribute('aria-selected',String(sidebarView===view));
  const area=$('sessions'),scroll=area.scrollTop;area.replaceChildren();
  const buckets=[...visibleProjects.map(p=>({id:p.id,name:p.name,archived:!!p.archived})).sort((a,b)=>a.name.localeCompare(b.name)),{id:null,name:'Ungrouped'}];
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
        if(project.id===null&&!items.length&&visibleProjects.length)continue;
        const child=treeGroup(`host:${host.id}:project:${project.id||'ungrouped'}`,project.name,items,{projectId:project.id,level:1,archived:project.archived});
        addSessions(child.children,items);top.children.append(child.group);
      }
      area.append(top.group);
    }
  }else{
    for(const project of buckets){
      const projectItems=visibleSessions.filter(s=>(s.projectId||null)===project.id);
      if(project.id===null&&!projectItems.length&&visibleProjects.length)continue;
      const top=treeGroup('project:'+(project.id||'ungrouped'),project.name,projectItems,{projectId:project.id,archived:project.archived});
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
  if(active){const current=sessions.find(s=>s.id===active.id);if(current){active.projectId=current.projectId;active.hostId=current.hostId;active.automation=current.automation;active.archived=current.archived;}}
  sidebar();workspace?.sync(catalog,sessions);
}
function openProjectDialog(project=null){
  if(savingProject)return;
  editingProject=project?.id||null;$('project-dialog-title').textContent=project?'Project settings':'New project';
  $('project-name').value=project?.name||'';$('project-communication-role').value=project?.defaultCommunicationRole||'';
  $('project-communication-role').querySelector('option[value=""]').textContent='Use global default · '+communicationLabels[config.defaultCommunicationRole||'participant'];
  $('project-error').textContent='';$('delete-project').hidden=!project;$('archive-project').hidden=!project;$('open-controllers').hidden=!project;
  $('archive-project').textContent=project?.archived?'Unarchive project':'Archive project';
  policyEditor.open(editingProject);$('project-dialog').showModal();$('project-name').focus();
}
function resetTranscript(){transcript=[];tools=new Map();approvals=new Map();assistant=null;nodes.clear();turnEnds=new Map();workLogs?.reset();$('messages').replaceChildren();$('approval-area').replaceChildren();}
// The desktop header and the phone session bar both show the session title.
function setTitle(title){$('header-title').textContent=title;$('session-heading-title').textContent=title;}
// Phone navigation: the session browser and a chat are separate history
// entries, so the system Back button returns from a chat to the browser.
const phone=()=>matchMedia('(max-width:650px)').matches;
function showBrowser(){sessionMenu.close();composerMenu.close();document.body.classList.add('menu-open');}
function enterChat(){
  if(!phone()||history.state?.agentworks==='chat')return;
  if(!history.state?.agentworks)history.replaceState({agentworks:'browser'},'');
  history.pushState({agentworks:'chat'},'');
}
addEventListener('popstate',event=>{
  if(!phone()||$('conversation').hidden)return;
  const entry=event.state?.agentworks;sidePanel?.popstate(entry);
  if(entry==='chat'||entry==='panel')document.body.classList.remove('menu-open');else showBrowser();
});
function selectSession(s) {
  document.body.classList.remove('menu-open');enterChat();
  if(active?.id===s.id){$('welcome').hidden=true;$('conversation').hidden=false;workspace?.sessionSelected(s);sidePanel?.sessionChanged();return;}
  if(active){drafts[active.id]=$('prompt').value;sessionStorage.setItem('agentworks-drafts',JSON.stringify(drafts));scrollPositions.set(active.id,$('messages').scrollTop);}
  $('prompt').value=drafts[s.id]||'';restoreScroll=scrollPositions.get(s.id)??null;
  clearTimeout(retryTimer);generation++;socket?.close();socket=null;connected=false;
  active=s;status=s.status;modelConfig=null;permissionModes=null;availableCommands=[];commandsOpen=false;renderCommands();mentions.close();composerMenu.close();sessionMenu.close();resetTranscript();usageStatus.reset();inputQueue.reset();
  localStorage.setItem('agentworks-session',s.id);$('welcome').hidden=true;$('conversation').hidden=false;
  setTitle(s.title);$('provider-label').textContent=labels[s.provider];$('project-path').textContent=s.cwd;
  $('session-notice').hidden=true;document.body.classList.remove('menu-open');sidebar();updateControls();connect(generation);
  workspace?.sessionSelected(s);sidePanel?.sessionChanged();
  if(s.status==='suspended'&&!s.explicitlySuspended)resumeSession(s.id);
}
function connect(gen){
  if(gen!==generation||!active)return;
  $('connection').textContent='Connecting';
  const ws=new WebSocket(socketUrl(`/api/sessions/${active.id}/events`));socket=ws;
  ws.onopen=()=>{if(gen!==generation)return;connected=true;reconnectDelay=500;$('connection').textContent='Connected';updateControls();};
  ws.onmessage=event=>{
    if(gen!==generation)return;
    const e=JSON.parse(event.data);
    if(e.kind==='input_queue'){inputQueue.set(e.data);updateControls();return;}
    if(e.kind==='snapshot'){
      resetTranscript();usageStatus.reset();status=e.session.status;Object.assign(active,e.session);
      replaying=true;try{for(const item of e.events)if(item.kind!=='session_updated')consume(item);}finally{replaying=false;}
      Object.assign(active,e.session);setTitle(active.title);
      active.continuation=e.session.continuation;
      approvals=new Map(e.approvals.map(a=>[a.id,a]));status=e.session.status;modelConfig=e.models;syncModelLabel();permissionModes=e.permissionModes;if(status==='suspended'&&!e.session.explicitlySuspended)resumeSession(active.id);availableCommands=e.commands||[];renderCommands();inputQueue.set(e.session.inputQueue);inputQueue.observe(status,{live:false});
      if(e.storageError){$('session-notice').textContent=e.storageError;$('session-notice').hidden=false;}
      if(e.truncated){$('session-notice').textContent='Showing recent output. The full transcript is saved on this host.';$('session-notice').hidden=false;}
    }else consume(e,true);
    scheduleDraw();updateControls();renderContinuation();usageStatus.render();
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
function consume(e,live=false){
  const d=e.data;
  usageStatus.consume(e,{live});
  if(d?.continuationState&&active)active.continuation=d.continuationState;
  {const t=timeoutControls?.notice(e.kind,d);if(t){transcript.push({id:uuid(),type:'notice',text:t});assistant=null;}}
  if(e.kind==='session_updated'){
    Object.assign(active,d);setTitle(active.title);const row=sessions.find(s=>s.id===active.id);if(row)Object.assign(row,d);sidebar();
  }
  else if(e.kind==='continuation'){renderContinuation();}
  else if(e.kind==='models'){modelConfig=d;syncModelLabel();}
  else if(e.kind==='permission_modes'){permissionModes=d;}
  else if(e.kind==='commands'){availableCommands=Array.isArray(d)?d:[];renderCommands();}
  else if(e.kind==='user'){assistant=null;if(d.inboxDelivery)transcript.push({id:uuid(),type:'notice',text:'Automatic inbox turn authorized by you; peer messages are untrusted context.'});else if(d.initiatedBy)transcript.push({id:uuid(),type:'notice',text:d.initiatedBy.startsWith('controller:')?'Turn started by external controller '+(d.initiatorTitle||d.initiatedBy):'Turn started by agent session '+(sessions.find(s=>s.id===d.initiatedBy)?.title||d.initiatedBy)});addMessage('user',d.text).seq=e.seq;}
  else if(e.kind==='status'){
    status=d.status;if(!replaying)inputQueue.observe(status,{live});if(active){active.status=status;if(d.explicitlySuspended!==undefined)active.explicitlySuspended=d.explicitlySuspended;const s=sessions.find(s=>s.id===active.id);if(s)s.status=status;}sidebar();
  }else if(e.kind==='approval'){approvals.set(d.id,d);}
  else if(e.kind==='approval_resolved'){
    const pending=approvals.get(d.id);approvals.delete(d.id);
    if(pending?.kind==='question'){
      // L-02: record the outcome in the transcript; drafts end with the question.
      questions.forget(d.id);assistant=null;
      const asked=(pending.request?.message||'Question').slice(0,120);
      transcript.push({id:uuid(),type:'notice',text:`${asked} — ${d.action==='accept'?'answer sent':d.action==='decline'?'skipped':d.action==='cancel'?'cancelled by you':d.withdrawn?'withdrawn by the agent':'cancelled because the turn ended'}.`});
    }
  }
  else if(e.kind==='interrupted'){transcript.push({id:uuid(),type:'notice',text:d.message,interrupted:true});assistant=null;}
  else if(e.kind==='storage_error'){status='error';$('session-notice').textContent=d.message;$('session-notice').hidden=false;}
  else if(e.kind==='error'){transcript.push({id:uuid(),type:'error',text:d.message});assistant=null;}
  else if(e.kind==='turn_end'){assistant=null;turnEnds.set(transcript.at(-1)?.id,{seq:e.seq,stopReason:d?.timedOut?`timeout_${d.timedOut}`:d?.stopReason});}
  else if(e.kind==='fork'){
    // L-08: the carried transcript belongs to another session; say so plainly.
    transcript.push({id:uuid(),type:'notice',
      text:`Forked from “${d.parentTitle}” after ${d.boundaryTurn===1?'its first finished turn':`turn ${d.boundaryTurn}`}${d.parentProvider?` (${labels[d.parentProvider]||d.parentProvider})`:''}. The conversation below is recorded history. Context handoff uses the chosen native or fresh-session method.`});
    assistant=null;
  }
  else if(e.kind==='fork_mode'){assistant=null;transcript.push({id:uuid(),type:'notice',text:d.mode==='native'?'Provider native fork created at the recorded boundary.':`${d.detail}${d.truncated?' Available source history is truncated.':''}`});}
  else if(e.kind==='carried_context'){assistant=null;transcript.push({id:uuid(),type:'notice',text:'Context handoff dispatched with this message; interrupted delivery requires inspection.'});}
  else if(e.kind==='acp'){
    switch(d.sessionUpdate){
      case 'user_message_chunk':{
        assistant=null;if(d.content?.type==='text'){
          const last=transcript.at(-1);
          if(last?.replayed&&last.role==='user'&&last.providerMessageId===d.messageId)last.text+=d.content.text;
          else{const message=addMessage('user',d.content.text);message.replayed=true;message.providerMessageId=d.messageId;message.seq=e.seq;}
        }break;
      }
      case 'agent_message_chunk':{
        if(d.content?.type==='text'){
          if(!assistant||(d.messageId&&assistant.providerMessageId&&d.messageId!==assistant.providerMessageId))assistant=addMessage('assistant','');assistant.providerMessageId=d.messageId;assistant.referenceCwd=d.forkSourceCwd;assistant.text+=d.content.text;
        }else if(d.content?.type==='image'){
          transcript.push({id:uuid(),type:'image',content:d.content});assistant=null;
        }else if(d.content?.type==='resource_link'){
          assistant=null;addMessage('assistant',`[${d.content.name||'Open file'}](${d.content.uri})`).referenceCwd=d.forkSourceCwd;
        }break;
      }
      case 'tool_call':{
        assistant=null;let tool=tools.get(d.toolCallId);if(tool)Object.assign(tool,d,{type:'tool'});else{tool={...d,id:d.toolCallId,type:'tool'};tools.set(d.toolCallId,tool);transcript.push(tool);}observeTool(tool,{live:!replaying});break;
      }
      case 'tool_call_update':{
        let tool=tools.get(d.toolCallId);
        if(!tool){tool={id:d.toolCallId,type:'tool',title:'Tool activity'};tools.set(d.toolCallId,tool);transcript.push(tool);}Object.assign(tool,d,{type:'tool'});observeTool(tool,{live:!replaying});break;
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
function updateControls(){questions.refresh();
  $('archive-session').textContent=active?.archived?'Unarchive session':'Archive session';
  $('suspend-session').disabled=!connected||status==='suspended'||status==='closed';
  $('resume-session').hidden=!['suspended','error'].includes(status);
  $('resume-session').disabled=!connected;
  $('show-commands').disabled=!connected||status!=='idle';
  const usage=availableCommands.find(c=>c.name===(active?.provider==='claude'?'usage':'status'));
  $('show-usage').hidden=!['claude','codex'].includes(active?.provider)||!usage;
  $('show-usage').disabled=!connected||status!=='idle';
  const modePicker=$('permission-mode-picker');modePicker.hidden=!permissionModes?.options?.length;
  const modeSignature=JSON.stringify(permissionModes);
  if(modePicker.dataset.config!==modeSignature){
    modePicker.replaceChildren(...(permissionModes?.options||[]).map(o=>{const option=document.createElement('option');option.value=o.value;option.textContent=o.name||o.value;return option;}));
    modePicker.value=permissionModes?.currentValue||'';modePicker.dataset.config=modeSignature;
  }
  modePicker.disabled=!connected||status!=='idle';
  $('permission-mode-note').textContent=permissionModes?.defaultFallback||'';
  $('permission-mode-note').hidden=!permissionModes?.defaultFallback;
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
  $('session-state').textContent=status==='waiting'?([...approvals.values()].some(a=>a.kind==='question')?'Needs your answer':'Needs your approval'):status==='idle'?'Ready':status[0]?.toUpperCase()+status.slice(1);
  // Busy sessions keep the composer usable so input can be queued without losing the draft.
  $('prompt').disabled=!connected||!(status==='idle'||busy);$('send').disabled=!connected||!(status==='idle'||busy);
  $('send').setAttribute('aria-label',busy?'Queue message':'Send message');$('send').textContent=busy?'＋':'↑';$('send').title=busy?'Queue to send after this turn ends':'';
  inputQueue.render();inputQueue.renderActivity();$('stop').hidden=!busy&&!['starting','resuming','configuring'].includes(status)&&!active?.continuation?.enabled;$('stop').disabled=!connected||status==='cancelling';
  $('prompt').placeholder=status==='starting'?'Starting your agent…':status==='waiting'?'Respond to the permission request above…':busy?'Your agent is working… type to queue a follow-up':status==='error'?'Session unavailable. Use Resume session to try again.':'What would you like to work on?';
}
function scheduleDraw(){if(!drawTimer)drawTimer=setTimeout(()=>{drawTimer=null;draw();},70);}
function fileUrl(path,download=false){return appUrl(`/api/sessions/${active.id}/file?path=${encodeURIComponent(path)}${download?'&download=true':''}`);}
function localReference(href,base=''){
  if(!href||href.startsWith('#'))return null;
  if(/^file:/i.test(href)){try{const url=new URL(href);return ['','localhost'].includes(url.hostname)?decodeURIComponent(url.pathname):null;}catch{return null;}}
  if(/^[a-z][a-z0-9+.-]*:/i.test(href)||href.startsWith('//'))return null;
  const decoded=decodeURIComponent(href.split('#')[0]);
  return decoded.startsWith('/')?decoded:base+decoded;
}
function decorateMarkdown(node,base=''){
  for(const a of node.querySelectorAll('a')){
    const path=localReference(a.getAttribute('href'),base);
    if(path!==null){a.href='#';a.onclick=e=>{e.preventDefault();if(active)workspace?.openSideFile(active,path);};}
    else if(/^file:/i.test(a.getAttribute('href')||''))a.removeAttribute('href');
    else {a.target='_blank';a.rel='noopener noreferrer';}
  }
  for(const img of node.querySelectorAll('img')){
    const path=localReference(img.getAttribute('src'),base);
    if(path!==null){img.src=fileUrl(path);img.classList.add('opens-preview');img.title='Open in the side panel';img.onclick=()=>{if(active)workspace?.openSideFile(active,path);};}
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
      message.html=result.html;body.innerHTML=result.html;body.classList.add('markdown');decorateMarkdown(body,message.referenceCwd?message.referenceCwd.replace(/\/$/,'')+'/':'');display.enhance(body,message.id);
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
        node=el('article','message '+item.role);node.dataset.messageId=item.id;node.tabIndex=0;node.append(el('div','avatar',item.role==='user'?'Y':icons[active.provider]));
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
  const pending=$('approval-area');for(const node of [...pending.children])if(!node.classList.contains('question'))node.remove();
  workLogs.apply(area,transcript,nodes,{sessionIdle:['idle','closed','suspended','error'].includes(status),scope:active.id,turnEnds});
  questions.sync(pending,[...approvals].filter(([,a])=>a.kind==='question'));
  for(const [id,{kind,request}]of approvals){
    if(kind==='question')continue;
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
$('new-session').onclick=()=>openCreate();$('welcome-new').onclick=()=>openCreate();
$('view-hosts').onclick=()=>setSidebarView('hosts');$('view-projects').onclick=()=>setSidebarView('projects');
$('new-project').onclick=()=>openProjectDialog();$('create-project-new').onclick=()=>openProjectDialog();
$('dismiss-project').onclick=()=>{if(!savingProject)$('project-dialog').close();};
$('project-dialog').addEventListener('cancel',event=>{if(savingProject)event.preventDefault();});
$('create-project').onchange=()=>{const remembered=readStorage(localStorage,'agentworks-project-locations',{})[$('create-project').value||'ungrouped'];if(remembered)$('directory').value=remembered;worktrees.projectChanged();updateCommunicationDefault();};
$('project-form').onsubmit=async event=>{
  event.preventDefault();if(savingProject)return;savingProject=true;
  for(const id of ['save-project','project-name','project-default-mode','project-require-worktree','project-communication-role','archive-project','delete-project','dismiss-project'])$(id).disabled=true;
  $('project-error').textContent='';
  try{
    const policy=policyEditor.value();
    const project=await api(editingProject?`/api/projects/${editingProject}`:'/api/projects',{method:editingProject?'PATCH':'POST',body:JSON.stringify({name:$('project-name').value,defaultCommunicationRole:$('project-communication-role').value||null})});
    editingProject=project.id;$('delete-project').hidden=false;$('archive-project').hidden=false;
    try{await api(`/api/projects/${project.id}/workspace-policy`,{method:'PUT',body:JSON.stringify(policy)});}
    catch(error){throw new Error(`Project name saved. Workspace policy was not saved: ${error.message}. Retry Save project.`);}
    await refreshSessions();if($('create-dialog').open){$('create-project').value=project.id;worktrees.projectChanged();updateCommunicationDefault();}$('project-dialog').close();
  }catch(error){$('project-error').textContent=error.message;}finally{savingProject=false;for(const id of ['save-project','project-name','project-default-mode','project-require-worktree','project-communication-role','archive-project','delete-project','dismiss-project'])$(id).disabled=false;}
};
// Archive and delete apply to the project's sessions too unless the user keeps them in Ungrouped.
let removingProject=null;
function openProjectRemove(action,project=projects.find(p=>p.id===editingProject)){
  if(!project)return;
  const count=sessions.filter(s=>s.projectId===project.id).length;
  removingProject={id:project.id,action,count};
  const verb=action==='delete'?'Delete':'Archive';
  $('project-remove-title').textContent=`${verb} ${project.name}`;$('confirm-project-remove').textContent=`${verb} project`;
  $('project-remove-ungroup').checked=false;$('project-remove-error').textContent='';describeProjectRemove();
  $('project-remove-dialog').showModal();
}
function describeProjectRemove(){
  const {action,count}=removingProject,plural=count===1?'session':'sessions',verb=action==='delete'?'Deletes':'Hides';
  $('project-remove-description').textContent=$('project-remove-ungroup').checked
    ?`${verb} the project. Its ${count} ${plural} move to Ungrouped and keep running.`
    :action==='delete'?`Deletes the project and the saved history of its ${count} ${plural}, stopping any running work. Workspace files and branches remain. This cannot be undone.`
    :`Hides the project and archives its ${count} ${plural}. Archiving never stops running work.`;
}
$('project-remove-ungroup').onchange=describeProjectRemove;
async function archiveProject(project){
  if(!project)return;
  if(!project.archived)return openProjectRemove('archive',project);
  try{await api(`/api/projects/${project.id}/archive`,{method:'POST',body:JSON.stringify({archived:false})});$('project-dialog').close();await refreshSessions();}catch(error){if($('project-dialog').open)$('project-error').textContent=error.message;else toast(error.message);}
}
$('archive-project').onclick=()=>archiveProject(projects.find(p=>p.id===editingProject));
$('delete-project').onclick=()=>openProjectRemove('delete');
$('dismiss-project-remove').onclick=()=>$('project-remove-dialog').close();
$('project-remove-form').onsubmit=async event=>{
  event.preventDefault();if(!removingProject)return;
  const {id,action}=removingProject,sessionsMode=$('project-remove-ungroup').checked?'ungroup':'cascade';
  $('confirm-project-remove').disabled=true;
  try{
    if(action==='delete')await api(`/api/projects/${id}?sessions=${sessionsMode}`,{method:'DELETE'});
    else await api(`/api/projects/${id}/archive`,{method:'POST',body:JSON.stringify({archived:true,sessions:sessionsMode})});
    $('project-remove-dialog').close();$('project-dialog').close();removingProject=null;
    await refreshSessions();
    if(active&&!sessions.some(s=>s.id===active.id))closeActive();
  }catch(error){$('project-remove-error').textContent=error.message;}finally{$('confirm-project-remove').disabled=false;}
};
$('session-project').onchange=async event=>{
  try{const updated=await api(`/api/sessions/${active.id}/project`,{method:'POST',body:JSON.stringify({projectId:event.target.value||null})});Object.assign(active,updated);workspace?.sessionSelected(active);await refreshSessions();}
  catch(error){toast(error.message);updateProjectControls();}
};
$('dismiss-create').onclick=()=>$('create-dialog').close();
$('menu').onclick=()=>document.body.classList.toggle('menu-open');$('session-home').onclick=()=>{if(history.state?.agentworks==='chat')history.back();else showBrowser();};$('session-search').onclick=()=>$('palette-search').click();$('scrim').onclick=()=>document.body.classList.remove('menu-open');
$('provider').onchange=()=>{providerNote();resetCreateModels();};
$('directory').onchange=resetCreateModels;
$('load-models').onclick=loadCreateModels;
$('create-model').onchange=()=>{modelPicked=true;};
$('show-commands').onclick=()=>{commandsOpen=!commandsOpen;renderCommands();};
$('show-usage').onclick=async()=>{
  if(!active||status!=='idle')return;
  $('show-usage').disabled=true;
  try{await api(`/api/sessions/${active.id}/prompt`,{method:'POST',body:JSON.stringify({text:active.provider==='claude'?'/usage':'/status',requestId:uuid()})});}
  catch(e){toast(e.message);}finally{updateControls();}
};
$('permission-mode-picker').onchange=async event=>{
  if(!active)return;
  const mode=event.target.value;event.target.disabled=true;
  try{await api(`/api/sessions/${active.id}/permission-mode`,{method:'POST',body:JSON.stringify({mode})});}
  catch(error){toast(error.message);updateControls();}
};
$('model-picker').onchange=async event=>{
  try{await api(`/api/sessions/${active.id}/model`,{method:'POST',body:JSON.stringify({model:event.target.value})});}
  catch(error){toast(error.message);event.target.value=modelConfig.currentValue;}
};
$('create-form').onsubmit=async event=>{
  event.preventDefault();if(creating||worktrees.blocked())return;creating=true;providerNote();$('create-error').textContent='';
  const fork=forkSource;
  try{
    const payload={resumePolicy:{enabled:$('create-resume-policy').checked},provider:$('provider').value,...worktrees.submission(),title:$('session-title').value,projectId:$('create-project').value||null,hostId:$('create-host').value,model:$('create-model').value,...($('create-agent-access').value?{agentAccess:{communicate:$('create-agent-access').value!=='off',...Object.fromEntries(['startSessions','promptSessions','cancelSessions','runAutomations'].map(key=>[key,$('create-agent-access').value==='coordinator']))}}:{})};
    if(fork)Object.assign(payload,{boundarySeq:$('fork-boundary').value?Number($('fork-boundary').value):null,context:$('fork-context').value,summary:$('fork-context').value==='summary'?$('fork-summary').value:null});
    const signature=JSON.stringify({source:fork?.id||null,payload});
    if(!pendingCreate||pendingCreate.signature!==signature)pendingCreate={signature,requestId:uuid()};
    sessionStorage.setItem('agentworks-pending-create',JSON.stringify(pendingCreate));
    const path=fork?`/api/sessions/${fork.id}/fork`:'/api/sessions';
    const created=await api(path,{method:'POST',body:JSON.stringify({...payload,requestId:pendingCreate.requestId})});
    pendingCreate=null;sessionStorage.removeItem('agentworks-pending-create');
    const locations=readStorage(localStorage,'agentworks-project-locations',{});locations[created.projectId||'ungrouped']=created.cwd;localStorage.setItem('agentworks-project-locations',JSON.stringify(locations));
    sessions.unshift(created);$('create-dialog').close();selectSession(created);
    if(fork)toast(`Forked from “${fork.title}”. Its recorded history and lineage are preserved; the selected context handoff is applied.`,'success');
  }catch(e){$('create-error').textContent=e.message;}finally{creating=false;providerNote();}
};
// L-05: one stable pending identity per session (survives switches, reloads and lost
// responses) shared by direct send and queueing, so a retry can never send twice.
const pendingSends=readStorage(sessionStorage,'agentworks-pending-sends',{});
function pendingFor(sid,text){
  if(pendingSends[sid]?.text!==text)pendingSends[sid]={text,requestId:uuid()};
  sessionStorage.setItem('agentworks-pending-sends',JSON.stringify(pendingSends));return pendingSends[sid];
}
// draft is the composer text; submission.text may carry resolved @mentions.
function settle(sid,submission,draft=submission.text){
  if(active?.id===sid&&$('prompt').value===draft)$('prompt').value='';
  if(drafts[sid]===draft)delete drafts[sid];sessionStorage.setItem('agentworks-drafts',JSON.stringify(drafts));
  if(pendingSends[sid]?.requestId===submission.requestId){delete pendingSends[sid];sessionStorage.setItem('agentworks-pending-sends',JSON.stringify(pendingSends));}
}
$('composer').onsubmit=async event=>{
  event.preventDefault();const text=$('prompt').value;if(!active||!text.trim())return;
  if(!BUSY.includes(status)&&status!=='idle')return;
  const sid=active.id,submission={...pendingFor(sid,mentions.expand(text))};
  $('send').disabled=true;
  try{
    if(BUSY.includes(status))await inputQueue.enqueue(sid,submission);
    else try{await api(`/api/sessions/${sid}/prompt`,{method:'POST',body:JSON.stringify(submission)});}
    catch(e){
      // Send→Enter race: the session became busy first. Queue the same intended input.
      if(e.status!==409||!/become idle/.test(e.message))throw e;
      await inputQueue.enqueue(sid,submission);
    }
    settle(sid,submission,text);
  }catch(e){toast(e.message);}finally{updateControls();}
};
$('prompt').oninput=()=>{mentions.update();commandsOpen=/^[/\$]\S*$/.test($('prompt').value);renderCommands();if(active){drafts[active.id]=$('prompt').value;sessionStorage.setItem('agentworks-drafts',JSON.stringify(drafts));}};
$('prompt').onkeydown=event=>{if(mentions.keydown(event))return;if(event.key==='Enter'&&!event.shiftKey&&!event.isComposing){event.preventDefault();$('composer').requestSubmit();}};
$('resume-session').onclick=()=>{if(active)resumeSession(active.id);};
$('stop').onclick=async()=>{try{await api(`/api/sessions/${active.id}/cancel`,{method:'POST',body:'{}'});}catch(e){toast(e.message);}};
async function deleteSession(session){
  if(!session)return;
  if(!confirm(`Permanently delete “${session.title}” and its saved transcript? Its harness will stop. Worktree files and branches remain. This cannot be undone.`))return;
  const id=session.id;
  try{
    await api(`/api/sessions/${id}`,{method:'DELETE'});
    if(active?.id===id)closeActive();
    await refreshSessions();
  }catch(e){toast(e.message);}
}
$('close-session').onclick=()=>deleteSession(active);
function closeActive(){generation++;socket?.close();clearTimeout(retryTimer);active=null;localStorage.removeItem('agentworks-session');$('conversation').hidden=true;$('welcome').hidden=false;setTitle('Overview');}
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
// Keep category contents mounted so switching sections preserves unsaved edits.
const settingsTabs=[...document.querySelectorAll('[data-settings-category]')];
function selectSettingsCategory(category){
  for(const tab of settingsTabs){
    const selected=tab.dataset.settingsCategory===category;
    tab.setAttribute('aria-selected',String(selected));tab.tabIndex=selected?0:-1;
    $(tab.getAttribute('aria-controls')).hidden=!selected;
  }
  $('settings-dialog').querySelector('.settings-content').scrollTop=0;
}
const settingsNarrow=matchMedia('(max-width:900px)');
const settingsOrientation=()=>$('settings-dialog').querySelector('[role="tablist"]').setAttribute('aria-orientation',settingsNarrow.matches?'horizontal':'vertical');
settingsOrientation();settingsNarrow.addEventListener('change',settingsOrientation);
for(const [index,tab] of settingsTabs.entries()){
  tab.onclick=()=>{selectSettingsCategory(tab.dataset.settingsCategory);tab.scrollIntoView({block:'nearest',inline:'nearest'});};
  tab.onkeydown=event=>{
    const previous=settingsNarrow.matches?'ArrowLeft':'ArrowUp',next=settingsNarrow.matches?'ArrowRight':'ArrowDown';
    let target;
    if(event.key===previous)target=(index+settingsTabs.length-1)%settingsTabs.length;
    else if(event.key===next)target=(index+1)%settingsTabs.length;
    else if(event.key==='Home')target=0;
    else if(event.key==='End')target=settingsTabs.length-1;
    else return;
    event.preventDefault();settingsTabs[target].click();settingsTabs[target].focus();
  };
}
// Native form validation must reveal invalid fields in another category before focusing them.
$('settings-form').addEventListener('invalid',event=>{
  const panel=event.target.closest('[role="tabpanel"]');
  if(panel)selectSettingsCategory(panel.id.replace('settings-panel-',''));
},true);
$('add-root').onclick=()=>addRoot().focus();
async function openSettings(){
  const timeoutsReady=timeoutControls.load();
  notifications.load().then(()=>notifications.mount($('notification-controls'))).catch(error=>notifications.mount($('notification-controls'),error));
  try{
    const settings=await api('/api/settings');$('settings-roots').replaceChildren();for(const root of settings.roots)addRoot(root);$('settings-error').textContent='';$('settings-saved').textContent='Save applies global defaults and host settings. Display and notifications save immediately.';
    $('default-communication-role').value=settings.defaultCommunicationRole||'participant';
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
      if(provider.id==='claude'){
        const modeLabel=el('label','','Claude default permission mode');modeLabel.htmlFor='claude-default-permission-mode';
        const mode=el('select');mode.id='claude-default-permission-mode';
        for(const [value,name] of [['auto','Auto'],['default','Default'],['acceptEdits','Accept edits'],['plan','Plan']]){
          const option=el('option','',name);option.value=value;mode.append(option);
        }
        mode.value=settings.claudeDefaultPermissionMode||'auto';
        const note=el('div','field-note',"Applies only to new Claude sessions. Auto uses Claude’s normal Auto mode. If the adapter does not advertise your choice, its current permission mode is kept and a note appears in Session options. Reconnected sessions keep their saved mode.");
        fields.append(modeLabel,mode,note);
      }
    }
    await timeoutsReady;
    $('settings-dialog').showModal();
    $('settings-dialog').querySelector('.settings-content').scrollTop=0;
    refreshService();
  }catch(e){toast(e.message);}
};
$('dismiss-settings').onclick=()=>$('settings-dialog').close();
$('settings-form').onsubmit=async event=>{
  event.preventDefault();$('save-settings').disabled=true;$('settings-error').textContent='';
  try{
    const defaultModels={};for(const input of $('default-model-fields').querySelectorAll('input')){if(input.value.trim())defaultModels[input.dataset.provider]=input.value.trim();}
    const timeouts=timeoutControls.pendingDefault();
    const settings=await api('/api/settings',{method:'PUT',body:JSON.stringify({roots:rootValues().filter(Boolean),defaultModels,claudeDefaultPermissionMode:$('claude-default-permission-mode')?.value,defaultCommunicationRole:$('default-communication-role').value,...(timeouts?{timeouts}:{})})});
    if(timeouts)timeoutControls.acceptDefault(timeouts);
    config.roots=settings.roots;config.defaultModels=settings.defaultModels;config.defaultCommunicationRole=settings.defaultCommunicationRole||'participant';applyRoots();
    $('settings-dialog').close();toast('Settings saved. They apply to new sessions.','success');
  }catch(e){$('settings-error').textContent='Host settings were not saved: '+e.message+' Display preferences are stored separately in this browser.';}finally{$('save-settings').disabled=false;}
};
document.addEventListener('keydown',event=>{if(event.key==='n'&&!event.ctrlKey&&!event.metaKey&&!event.altKey&&!['INPUT','TEXTAREA','SELECT'].includes(document.activeElement?.tagName)&&!document.querySelector('dialog[open]')){event.preventDefault();openCreate();}});
const diffView=setupDiff({api,root:$('side-diff')});
timeoutControls=setupTimeoutControls({$,api,getActive:()=>active});
workspace=createWorkspace({api,toast,labels,uuid,getConfig:()=>config,getActive:()=>active,selectSession,openCreate,refreshSessions,enhanceBlocks:display.enhance,openSettings,
  overview(){ $('conversation').hidden=true;$('welcome').hidden=false;localStorage.removeItem('agentworks-session'); },
  openPanel:tab=>sidePanel?.open(tab),
  appendDraft(text){if(!active)return;const current=$('prompt').value;$('prompt').value=current?current+'\n\n'+text:text;drafts[active.id]=$('prompt').value;sessionStorage.setItem('agentworks-drafts',JSON.stringify(drafts));},
});
sidePanel=setupSidePanel({panel:$('side-panel'),tabs:{files:$('side-tab-files'),diff:$('side-tab-diff')},triggers:{files:$('session-files'),diff:$('session-diff')},close:$('side-close'),renderers:{files:workspace.sideFiles,diff:diffView},getActive:()=>active,phone});
async function init(){
  config=await api('/api/status');
  $('host-address').textContent=location.host+' · Linux prototype';
  for(const p of config.providers){const option=el('option','',p.name+(p.authMode==='api_key'&&!p.keyPresent?' · API key needed':''));option.value=p.id;$('provider').append(option);}
  applyRoots();
  await refreshSessions();
  const saved=sessions.find(s=>s.id===localStorage.getItem('agentworks-session'));if(saved)selectSession(saved);workspace.restoreView();
  setInterval(()=>refreshSessions().catch(()=>{}),5000);
  notifications.start();
}
init().catch(error=>toast('Cannot reach the local server: '+error.message));

setupCommunication({api,getActive:()=>active,getSessions:()=>sessions,selectSession,toast,uuid});
setupInboxDelivery({api,getActive:()=>active,toast,uuid});

setupGitContext({api,getActive:()=>active});
const worktreeOwnership=setupWorktreeOwnership({api,toast});
setupControllers({api,toast,getProject:()=>editingProject});

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
    settings:openSettings,
    agents:()=>{if(active)$('session-agents').click();},
    workspaceInfo:()=>{if(active)$('session-workspace').click();},
    diff:()=>sidePanel.open('diff'),
    sessionFiles:()=>sidePanel.open('files'),
    lifecycle:()=>{if(active){$('lifecycle-menu').open=true;$('lifecycle-menu').querySelector('button').focus();}},
    forkSession:()=>{if(active)openFork();},
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
  $('continuation-label').textContent=p.enabled?'On':'Off';composerMenu.setFlag(!!p.enabled);
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
  try{const updated=await api(`/api/sessions/${id}`,{method:'PATCH',body:JSON.stringify({title:$('rename-title').value})});if(active?.id===id){Object.assign(active,updated);setTitle(updated.title);}await refreshSessions();$('rename-dialog').close();}catch(e){$('rename-error').textContent=e.message;}
};
async function archiveSession(session){
  if(!session)return;
  const id=session.id;
  try{const updated=await worktreeOwnership.archive(session);if(!updated)return;if(active?.id===id)Object.assign(active,updated);await refreshSessions();updateControls();}catch(e){toast(e.message);}
}
$('archive-session').onclick=()=>archiveSession(active);
$('fork-session').onclick=()=>openFork();
setupContextMenu({onError:message=>toast(message),resolve(target){
  const row=target.closest('.session-item');
  if(row){
    const session=sessions.find(s=>s.id===row.dataset.sessionId);if(!session)return null;
    return {origin:row,label:`Actions for ${session.title}`,actions:[
      {label:'Fork session…',run:()=>openFork(session)},
      {label:session.archived?'Unarchive session':'Archive session',run:()=>archiveSession(session)},
      {label:'Delete history…',danger:true,run:()=>deleteSession(session)},
    ]};
  }
  const heading=target.closest('.tree-heading[data-project-id]');
  if(heading){
    const project=projects.find(p=>p.id===heading.dataset.projectId);if(!project)return null;
    return {origin:heading,label:`Actions for ${project.name}`,actions:[
      {label:'Edit project…',run:()=>openProjectDialog(project)},
      {label:project.archived?'Unarchive project':'Archive project…',run:()=>archiveProject(project)},
      {label:'Delete project…',danger:true,run:()=>openProjectRemove('delete',project)},
    ]};
  }
  if(target.closest('a,button,input,textarea,select,img,pre,code')||window.getSelection()?.toString())return null;
  const node=target.closest('#messages article.message');
  const message=node&&transcript.find(m=>m.id===node.dataset.messageId);
  if(!message?.text)return null;
  return {origin:node,label:'Message actions',actions:[
    {label:'Copy message',run:async()=>{if(!await display.copy(message.text))throw new Error('Could not copy the message. Select its text and copy manually.');toast('Message copied.','success');}},
    {label:'Quote in reply',run:()=>{
      const quote=message.text.split('\n').map(line=>'> '+line).join('\n');
      const prompt=$('prompt');prompt.value=(prompt.value?prompt.value+'\n\n':'')+quote+'\n\n';
      prompt.dispatchEvent(new Event('input'));prompt.focus();
    }},
  ]};
}});
$('suspend-session').onclick=async()=>{
  const id=active?.id;if(!id)return;
  try{const updated=await api(`/api/sessions/${id}/suspend`,{method:'POST',body:'{}'});if(active?.id===id){Object.assign(active,updated);status=updated.status;updateControls();}await refreshSessions();}catch(e){toast(e.message);}
};

$('dismiss-lifecycle').onclick=()=>{$('lifecycle-menu').open=false;$('lifecycle-menu').querySelector('summary').focus();};
$('lifecycle-menu').addEventListener('keydown',event=>{if(event.key==='Escape'){$('dismiss-lifecycle').click();event.stopPropagation();}});

// Polling never submits or replays a restart request. Identity is server-owned and durable.
const serviceSection=document.createElement('fieldset');serviceSection.id='service-controls';
serviceSection.innerHTML=`<legend>Service · all projects</legend><p id="service-status" role="status" aria-live="polite">Loading service status…</p><label for="service-mode">Restart behavior</label><select id="service-mode"><option value="wait">Wait until all work is idle</option><option value="interrupt">Interrupt ALL active sessions</option></select><label for="service-expiry">Drain expiry (seconds, 5–900)</label><input id="service-expiry" type="number" min="5" max="900" value="300"><label for="service-release">Prepared release (blank keeps current executable)</label><input id="service-release" maxlength="128" placeholder="Immutable release basename"><button type="button" id="service-restart">Restart service</button><button type="button" id="service-cancel" hidden>Cancel pending restart</button><p id="service-error" class="error-text" role="alert"></p>`;
$('settings-service-slot').replaceWith(serviceSection);
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
setInterval(refreshService,1000);
