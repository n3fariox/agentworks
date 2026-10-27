import {createBackgroundJobs,jobResultLines} from './background-jobs.js';
import {createSubagents} from './subagents.js';
import {setupTerminal} from './terminal.js';
import {hostEnrollment} from './host-enrollment.js';
import {appUrl, socketUrl, localState as localStorage, sessionState as sessionStorage} from './hosting.js';
import {setupInboxDelivery} from './inbox-delivery.js';
import {setupDiff} from './diff.js';
import {setupWorktreeControls,setupPolicyEditor} from './worktree-controls.js';
import {setupGitContext} from './git-context.js';
import {setupWorktreeOwnership} from './worktree-ownership.js';
import {setupControllers} from './controllers.js';
import {setupImageViewer} from './image-viewer.js';
import {setupChatReferences,renderUserReferences} from './chat-references.js';
import {createWorkspace} from './workspace.js';
import {setupCommunication} from './communication.js';
import {setupPalettes} from './palettes.js';
import {enhanceMermaid} from './mermaid.js';
import {setupDisplayPreferences} from './display-preferences.js';
import {setupWorkLogs,observeTool} from './work-logs.js';
import {setupUsageStatus} from './usage-status.js';
import {setupFlyout} from './flyout.js';
import {setupResize,setupPromptResize} from './resize-handle.js';
import {setupSidebarPin} from './sidebar-pin.js';
import {setupSidePanel} from './side-panel.js';
import {setupContextMenu} from './context-menu.js';
import {setupTimeoutControls} from './timeout-controls.js';
import {setupParserScripts} from './parser-scripts.js';
import {setupInputQueue,setupPromptRecovery,BUSY} from './input-queue.js';
import {setupNotifications} from './notifications.js';
import {setupQuestions} from './questions.js';
import {setupMentions} from './mentions.js';
import {setupAttachments} from './attachments.js';
import {setupModelProviders} from './model-providers.js';
import {setupDirectoryPicker,setupProjectDirectory} from './directory-picker.js';
const $ = id => document.getElementById(id);
// randomUUID is unavailable on plain HTTP remote origins; getRandomValues is supported.
const uuid = () => globalThis.crypto.randomUUID?.() ?? Array.from(crypto.getRandomValues(new Uint8Array(16)), byte => byte.toString(16).padStart(2, '0')).join('');
const labels = {goose:'Goose · embedded',demo:'Demo agent',claude:'Claude','claude-rust':'Claude · Rust ACP',codex:'Codex','codex-rust':'Codex · Rust ACP',opencode:'OpenCode'};
const icons = {goose:'◉',demo:'◈',claude:'✳','claude-rust':'✳',codex:'⌘','codex-rust':'⌘',opencode:'▣'};
function readStorage(storage,key,fallback){try{return JSON.parse(storage.getItem(key))??fallback;}catch{return fallback;}}
let workspace;
let projects=[],locations=[],hosts=[{id:'local',name:'Local',status:'connected'}];
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
const sidebarPin=setupSidebarPin({sidebar:$('sidebar'),button:$('sidebar-pin'),trigger:$('nav-sessions')});
setupResize({handle:$('side-panel-resize'),target:$('side-panel'),panel:$('side-panel'),variable:'--side-panel-width',key:'agentworks-side-panel-width',min:300,max:960,initial:420,edge:'left'});
const promptResize=setupPromptResize($('prompt'));
const promptRecovery=setupPromptRecovery({area:$('prompt'),storage:sessionStorage,getSession:()=>active?.id,onChange:()=>{$('prompt').dispatchEvent(new Event('input'));}});
let currentUserSeq=null;
let sidePanel=null;
setInterval(()=>usageStatus.render(),60000);
let timeoutControls=null,parserScripts=null;
let transcript=[], tools=new Map(), approvals=new Map(), assistant=null, status='starting';
const backgroundJobs=createBackgroundJobs({root:$('background-jobs'),api,getActive:()=>active,isConnected:()=>connected,onChange:()=>scheduleDraw()});
const subagents=createSubagents({jobResult:job=>[`${job.name||'Background job'} · ${backgroundJobs.label(job.state)}`,...jobResultLines(job)].join('\n'),append:()=>{},tabs:$('session-tabs'),panel:$('subagent-view'),messages:$('messages'),onSelect:view=>{sessionTerminal.select(view);scheduleDraw();},uuid,applyToolUpdate,renderToolActivity,renderMessage:(body,item)=>{if(item.role==='Agent')updateMarkdown(item,body);else body.textContent=item.text;},el});
// L-05 hook: activity indicator and explicit queued inputs (server-owned state).
const questions=setupQuestions({api,uuid:()=>uuid(),storage:localStorage,getActive:()=>active,isConnected:()=>connected,agentName:request=>subagents.name(request?.sessionId)||labels[active?.provider]||'the agent'});
const inputQueue=setupInputQueue({$,api,uuid:()=>uuid(),getActive:()=>active,getStatus:()=>status,toolActive:()=>[...tools.values()].some(t=>['pending','in_progress'].includes(t.status))});
let permissionModes=null,sessionConfig=null;
const mentions=setupMentions({prompt:$('prompt'),list:$('mention-list'),getActive:()=>active,getSessions:()=>sessions});
let replaying=false,workLogs=null,turnEnds=new Map();
// Older transcript pages. `storedEvents` holds the stored events behind the current transcript
// (older pages + snapshot window + live events, unique and ascending by seq) so prepending a
// page re-folds everything: a message or tool stream may straddle a page boundary.
// `older.mode`: 'none' (snapshot is complete), 'more' (server reported truncation) or
// 'begin' (the server confirmed there is nothing earlier).
let storedEvents=[],older={mode:'none',loading:false,error:''},rebuilding=false,itemKey=null,itemCount=0,eventTime=null;
let modelConfig=null, availableCommands=[], commandsOpen=false;
let discoveryGeneration=0;
let externalSessions=[], externalSelection=null, externalCursor=null, externalGeneration=0, externalLoading=false, externalImporting=false;
let externalFetch=Promise.resolve();
let drawTimer=null, retryTimer=null, reconnectDelay=500, creating=false;
// Last sequenced event applied to this transcript; a reconnect resumes after it.
let eventSeq=null;
const nodes=new Map();
let pendingCreate=readStorage(sessionStorage,'agentworks-pending-create',null);
// L-08: set only while the new-session dialog is pre-filled from an existing session.
let forkSource=null,forkGeneration=0,modelPicked=false,roleLibrary=[];
const worktrees=setupWorktreeControls({$,api,onChange:()=>{if(config)providerNote();}});
const policyEditor=setupPolicyEditor({$,api});
const directoryPicker=setupDirectoryPicker({$,api});
const projectDirectory=setupProjectDirectory({$,picker:directoryPicker,hosts:()=>hosts});
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
  const remote=createHost();
  if(remote){
    const available=remote.status==='connected'&&remote.worker?.providers?.includes(p.id);
    $('provider-note').textContent=available?`Uses ${labels[p.id]} and its provider configuration on ${remote.name}.`:`${labels[p.id]} is unavailable on ${remote.name}.`;
    $('create-submit').disabled=!available||!remote.roots?.length||(p.id==='goose'&&$('create-model-provider').disabled)||creating||worktrees.blocked()||!!(forkSource?.role&&!p.roles);
    return;
  }
  $('provider-note').textContent=p.id==='demo'?'Local test agent. No model calls, API key, or file changes.':p.id==='goose'?'Uses the model provider configured for Goose on this host.':p.id==='opencode'?(p.installed?'Native ACP. Choose an advertised free model. Availability and limits may change.':'Install OpenCode on this host or set AGENTWORKS_OPENCODE_PATH.'):!p.installed?(p.id==='codex-rust'?'Install native Codex on this host or set CODEX_PATH.':p.id==='claude-rust'?'Install native Claude Code on this host or set CLAUDE_CODE_EXECUTABLE.':'Adapter not installed. Run npm ci --ignore-scripts.'):p.authMode==='existing'?'Uses the existing agent sign-in on this host. If it has expired, sign in with the CLI and try again.':!p.keyPresent?`Set ${['claude','claude-rust'].includes(p.id)?'ANTHROPIC_API_KEY':'OPENAI_API_KEY or CODEX_API_KEY'} before starting the server.`:'API key available on the host. Messages use your provider account.';
  $('create-submit').disabled=!p.installed||(p.authMode==='api_key'&&!p.keyPresent)||(p.id==='goose'&&$('create-model-provider').disabled)||creating||worktrees.blocked()||!!(forkSource?.role&&!p.roles);
}
// Roles add instructions to the agent's system prompt; they never change Agentworks access.
function renderRoles(){
  const select=$('create-role'),previous=select.value;select.replaceChildren(new Option('No role',''));
  for(const [label,builtin] of [['Built-in',true],['Custom',false]]){
    const items=roleLibrary.filter(r=>r.builtin===builtin);if(!items.length)continue;
    const group=document.createElement('optgroup');group.label=label;
    for(const r of items)group.append(new Option(r.name,r.id));select.append(group);
  }
  select.value=roleLibrary.some(r=>r.id===previous)?previous:'';updateRoleNote();
}
function updateRoleNote(){
  const select=$('create-role'),p=config.providers.find(p=>p.id===$('provider').value);
  if(forkSource){select.disabled=true;select.value='';$('create-role-note').textContent=!forkSource.role?'The parent has no role.':p?.roles?`Forks keep the parent's role · ${forkSource.role.name}`:`${labels[p?.id]||'This agent'} cannot fork this session because it must keep the “${forkSource.role.name}” role.`;return;}
  select.disabled=!p?.roles;if(!p?.roles)select.value='';
  const role=roleLibrary.find(r=>r.id===select.value);
  $('create-role-note').textContent=!p?.roles?`${labels[p?.id]||'This agent'} does not accept role instructions yet.`:role?(role.description||'Custom role.')+' Added to the agent\'s system prompt.':'Adds instructions to the agent\'s system prompt. Roles do not change Agentworks access.';
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
  const remembered=readStorage(localStorage,'agentworks-project-locations',{});
  $('create-resume-policy').checked=false;
  $('create-role').value='';api('/api/roles').then(list=>{roleLibrary=Array.isArray(list)?list:[];renderRoles();}).catch(()=>{});
  $('create-agent-access').value='';updateCommunicationDefault();$('create-error').textContent='';$('directory').value=context.cwd||projectDirectoryOn('local',projectId)||workspaceContext?.cwd||remembered[projectId||'ungrouped']||(active?.hostId==='local'?active.cwd:null)||config.roots[0];$('session-title').value=context.title||'';
  if(context.provider&&$('provider').querySelector(`option[value="${context.provider}"]`))$('provider').value=context.provider;
  $('fork-boundary').replaceChildren(new Option('Latest completed turn at submission',''));$('fork-context').value='auto';$('fork-summary').value='';
  renderForkHeader();fillCreateHosts(context.hostId||'local',context.cwd);providerNote();updateRoleNote();
  // Forking suggests its own worktree so the new session starts from this one's
  // position; the base revision is filled in from Git once the policy has loaded.
  worktrees.projectChanged(context.workspaceMode);
  $('create-dialog').showModal();
  return generation;
}
// Remote hosts appear once connected; sessions there use the host's own
// working directories and workspace policy.
function fillCreateHosts(selected,cwd){
  const select=$('create-host');select.replaceChildren(new Option('Local','local'));
  for(const host of hosts.filter(h=>h.kind==='remote')){
    const option=new Option(host.name+(host.status==='connected'?'':` · ${hostState(host).toLowerCase()}`),host.id);
    option.disabled=host.status!=='connected';select.append(option);
  }
  select.value=[...select.options].some(o=>o.value===selected&&!o.disabled)?selected:'local';
  select.disabled=false;
  createHostChanged(select.value===selected?(cwd||(select.value==='local'?$('directory').value:undefined)):undefined);
}
function createHost(){return hosts.find(h=>h.id===$('create-host').value&&h.kind==='remote')||null;}
// A project's saved directory, when it is on this host. Sessions start there
// unless the person chooses another directory.
function projectDirectoryOn(hostId,projectId){
  const id=projects.find(p=>p.id===projectId)?.defaultLocationId;
  const location=id&&locations.find(l=>l.id===id);
  return location&&location.hostId===hostId?location.path:null;
}
// The last source directory chosen in this browser for a project on a host.
function rememberedDirectory(){
  const host=$('create-host').value||'local',project=$('create-project').value||'ungrouped';
  return readStorage(localStorage,'agentworks-project-locations',{})[host==='local'?project:host+':'+project];
}
function createHostChanged(cwd){
  const host=createHost(),roots=host?host.roots||[]:config.roots;
  $('directory').value=cwd||projectDirectoryOn($('create-host').value||'local',$('create-project').value||null)||rememberedDirectory()||roots[0]||'';
  $('create-resume-policy').disabled=false;
  worktrees.setHost(host?.id);
  $('project-roots').replaceChildren(...roots.map(root=>{const option=el('option');option.value=root;return option;}));
  if(host){
    $('roots-note').textContent=roots.length?`Working directories on ${host.name}: ${roots.join(', ')}. Worktrees are created on this host. File and Git views are local only.`:`Add working directories to ${host.name} first (⋯ on the host).`;
  }else{
    $('roots-note').textContent='Allowed roots: '+config.roots.join(', ');
  }
  resetCreateModels();providerNote();modelProviders.loadCreate(forkSource?.provider===$('provider').value&&forkSource?.hostId===($('create-host').value||'local')?forkSource.modelProvider:undefined);
}
$('create-host').onchange=()=>createHostChanged();
$('directory-browse').onclick=async()=>{
  const hostId=$('create-host').value||'local',host=createHost();
  const path=await directoryPicker.pick({hostId,hostName:host?.name||'Local',start:$('directory').value.trim()});
  if(path){$('directory').value=path;$('directory').dispatchEvent(new Event('input'));$('directory').dispatchEvent(new Event('change'));}
};
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
  const generation=openCreate({fork:{id:parent.id,title:parent.title,provider:parent.provider,hostId:parent.hostId||'local',modelProvider:parent.modelProvider||null,cwd:parent.cwd,model:(active?.id===parent.id?modelConfig?.currentValue:null)||parent.model||null,role:parent.role||null,turns:0},
    hostId:parent.hostId||'local',projectId:parent.projectId??null,cwd:parent.cwd,title:`${parent.title} (fork)`,provider:parent.provider,workspaceMode:'newWorktree'});
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
  const carried=forkSource?.model&&forkSource.provider===$('provider').value&&(forkSource.modelProvider||null)===modelProviders.selection()?forkSource.model:null;
  const defaultId=carried||modelProviders.defaultModel()||config.defaultModels?.[$('provider').value];
  const option=el('option','',carried?`From “${forkSource.title}” · ${carried}`:defaultId?`Saved default · ${defaultId}`:'Harness default');option.value=defaultId||'';select.append(option);
  const remote=createHost();$('load-models').disabled=false;
  $('model-note').textContent=remote?`Load available models from ${remote.name} without sending a prompt.`:'Load available models from the agent without sending a prompt.';
}
async function loadCreateModels(){
  const gen=++discoveryGeneration,provider=$('provider').value,cwd=$('directory').value,hostId=$('create-host').value;
  modelPicked=true;
  $('load-models').disabled=true;$('model-note').textContent='Asking the harness for models…';
  try{
    const result=await api('/api/models',{method:'POST',body:JSON.stringify({provider,cwd,modelProvider:modelProviders.selection(),...(hostId!=='local'?{hostId}:{})})});
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
function importHost(){return hosts.find(h=>h.id===$('import-host').value&&h.kind==='remote')||null;}
function importProviders(){
  const host=importHost(),selected=$('import-provider').value||active?.provider||'claude';
  $('import-provider').replaceChildren();
  for(const provider of config.providers){const option=el('option','',provider.name);option.value=provider.id;option.disabled=host?!host.worker?.providers?.includes(provider.id):!provider.installed;$('import-provider').append(option);}
  const available=[...$('import-provider').options].filter(o=>!o.disabled);
  $('import-provider').value=available.some(o=>o.value===selected)?selected:available[0]?.value||selected;
}
function openImport(){
  if(!config)return;
  const selectedHost=$('create-dialog').open?$('create-host').value:active?.hostId||'local';
  $('create-dialog').close();updateProjectControls();
  $('import-host').replaceChildren(new Option('Local','local'));
  for(const host of hosts.filter(h=>h.kind==='remote')){const option=new Option(host.name,host.id);option.disabled=host.status!=='connected'||!host.roots?.length;$('import-host').append(option);}
  $('import-host').value=[...$('import-host').options].some(o=>o.value===selectedHost&&!o.disabled)?selectedHost:'local';
  importProviders();$('import-project').value=active?.projectId||'';
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
  for(const id of ['import-host','import-provider','import-source','import-directory','import-project','import-search'])$(id).disabled=externalImporting;
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
  const gen=++externalGeneration,provider=$('import-provider').value,cwd=$('import-directory').value.trim(),hostProfile=$('import-source').value==='host',hostId=$('import-host').value;
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
      result=await api('/api/external-sessions',{method:'POST',body:JSON.stringify({provider,cwd:cwd||null,cursor:more?externalCursor:null,hostProfile,...(hostId!=='local'?{hostId}:{})})});
    }finally{release();}
    if(gen!==externalGeneration)return;
    const seen=new Set(externalSessions.map(s=>s.sessionId));for(const session of result.sessions||[])if(!seen.has(session.sessionId)){externalSessions.push(session);seen.add(session.sessionId);}
    externalCursor=result.nextCursor||null;
  }catch(e){if(gen===externalGeneration)$('import-error').textContent=e.message;}
  finally{if(gen===externalGeneration){externalLoading=false;renderExternal();externalControls();}}
}
$('create-import').onclick=openImport;
$('dismiss-import').onclick=()=>{$('import-dialog').close();externalGeneration++;};
function changeExternalAgent(){
  $('import-search').value='';resetExternal();browseExternal();
}
$('import-host').onchange=()=>{importProviders();$('import-directory').value='';changeExternalAgent();};
$('import-provider').onchange=changeExternalAgent;$('import-source').onchange=changeExternalAgent;$('import-directory').oninput=resetExternal;
$('import-search').oninput=renderExternal;$('browse-sessions').onclick=()=>browseExternal();$('more-external').onclick=()=>browseExternal(true);
$('import-form').onsubmit=async event=>{
  event.preventDefault();if(!externalSelection||externalImporting||externalLoading)return;
  const selection=externalSelection;externalImporting=true;externalControls();$('import-error').textContent='';
  try{
    const created=await api('/api/sessions/import',{method:'POST',body:JSON.stringify({provider:$('import-provider').value,providerSessionId:selection.sessionId,cwd:selection.cwd,title:selection.title||'Imported session',projectId:$('import-project').value||null,hostProfile:$('import-source').value==='host',...($('import-host').value!=='local'?{hostId:$('import-host').value}:{})})});
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
function hostState(host){
  if(host.kind!=='remote')return 'Connected';
  return {connected:'Connected',connecting:'Connecting…',offline:'Offline'}[host.status]||host.status;
}
function hostTitle(host){
  if(host.kind!=='remote')return 'Connected';
  const w=host.worker;
  return [hostState(host),'ssh '+host.destination,w?.hostname,w?.platform,w?.providers?.length?'providers: '+w.providers.join(', '):'',w?.version?'v'+w.version:'',host.status==='offline'?host.detail:''].filter(Boolean).join(' · ');
}
function hostNote(host){
  if(host.status==='offline')return host.detail||'Offline.';
  if(host.status==='connecting')return host.detail?'Reconnecting. Last attempt: '+host.detail:'Connecting over SSH…';
  return host.roots?.length?'Connected. No sessions yet.':'Connected. Add working directories (⋯) to start sessions here.';
}
async function hostAction(host,action){
  if(action==='remove'&&!confirm(`Remove host ${host.name}? Its worker stops; nothing on that machine is deleted.`))return;
  try{await api(`/api/hosts/${host.id}`+(action==='remove'?'':'/reconnect'),{method:action==='remove'?'DELETE':'POST'});await refreshSessions();}catch(e){toast(e.message);}
}
let terminalHost=null;
function openHostTerminal(host){terminalHost=host;localStorage.setItem('agentworks-terminal-host',host.id);workspace.showView('host-terminal');sidebar();}
function treeGroup(key,label,items,{host=null,projectId=null,level=0,archived=false}={}){
  const group=el('details','tree-group level-'+level);group.dataset.group=key;
  const stateKey=sidebarView+':'+key;group.open=!collapsedGroups.has(stateKey);
  const heading=el('summary','tree-heading');if(!host&&projectId){heading.dataset.projectId=projectId;heading.setAttribute('aria-haspopup','menu');}heading.append(el('span','tree-chevron','›'));
  heading.append(el('span',host?'tree-icon host':'tree-icon',host?'▤':'◇'),el('span','tree-label',label),badges(items));
  if(archived)heading.append(el('span','group-total','Archived'));
  if(host){const dot=el('span','connection-dot '+(host.status||'connected'));dot.title=hostTitle(host);dot.setAttribute('aria-label',hostTitle(host));heading.append(dot);}
  if(!host&&projectId){
    const rename=el('button','group-action','⋯');rename.type='button';rename.setAttribute('aria-label','Edit project '+label);
    rename.onclick=event=>{event.preventDefault();event.stopPropagation();openProjectDialog(projects.find(p=>p.id===projectId));};heading.append(rename);
  }
  if(host){
    const terminal=el('button','group-action','›_');terminal.type='button';terminal.setAttribute('aria-label','Open terminal on '+label);terminal.title='Open terminal';
    terminal.setAttribute('aria-pressed',String(document.body.dataset.workspaceView==='host-terminal'&&terminalHost?.id===host.id));
    terminal.onclick=event=>{event.preventDefault();event.stopPropagation();openHostTerminal(host);};heading.append(terminal);
  }
  if(host?.kind==='remote'){
    // Link controls and session creation belong to the owning host.
    if(level===0){
      const actions=[['roots','⋯','Working directories on '+label],['reconnect','↻','Reconnect '+label]];if(!items.length)actions.push(['remove','×','Remove host '+label]);
      if(host.status==='connected'&&host.roots?.length)actions.unshift(['create','＋','New session in '+label]);
      actions.forEach(([action,text,name],index)=>{
        const button=el('button','group-action'+(index?'':' add-session'),text);button.type='button';button.setAttribute('aria-label',name);
        button.onclick=event=>{event.preventDefault();event.stopPropagation();
          if(action==='create')openCreate({projectId,hostId:host.id});else if(action==='roots')openHostRoots(host);else hostAction(host,action);};
        heading.append(button);
      });
    }else if(host.status==='connected'&&host.roots?.length){
      const add=el('button','group-action add-session','＋');add.type='button';add.setAttribute('aria-label','New session in '+label);
      add.onclick=event=>{event.preventDefault();event.stopPropagation();openCreate({projectId,hostId:host.id});};heading.append(add);
    }
  }else{
    const add=el('button','group-action add-session','＋');add.type='button';add.setAttribute('aria-label','New session in '+label);
    add.onclick=event=>{event.preventDefault();event.stopPropagation();openCreate({projectId});};heading.append(add);
  }
  const children=el('div','tree-children');group.append(heading,children);
  group.ontoggle=()=>{if(group.open)collapsedGroups.delete(stateKey);else collapsedGroups.add(stateKey);localStorage.setItem('agentworks-tree-collapsed',JSON.stringify([...collapsedGroups]));};
  return {group,children};
}
function sessionButton(s){
  const button=el('button','session-item'+(active?.id===s.id&&document.body.dataset.workspaceView!=='host-terminal'?' selected':''));button.type='button';button.dataset.sessionId=s.id;button.title=s.cwd;
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
  $('section-title').textContent=sidebarView==='hosts'?'HOSTS':'SESSIONS';
  $('session-count').textContent=sidebarView==='hosts'?hosts.length:visibleSessions.length;
  for(const view of ['hosts','projects'])$('view-'+view).setAttribute('aria-selected',String(sidebarView===view));
  const sectionAdd=sidebarView==='hosts'?'Add host':'New project';$('new-project').setAttribute('aria-label',sectionAdd);$('new-project').title=sectionAdd;
  const area=$('sessions'),scroll=area.scrollTop;area.replaceChildren();
  const buckets=[...visibleProjects.map(p=>({id:p.id,name:p.name,archived:!!p.archived})).sort((a,b)=>a.name.localeCompare(b.name)),{id:null,name:'Ungrouped'}];
  function addSessions(container,items){
    for(const session of items)container.append(sessionButton(session));
    if(!items.length)container.append(el('div','empty-group','No sessions yet'));
  }
  if(sidebarView==='hosts'){
    for(const host of hosts){
      const hostItems=visibleSessions.filter(s=>s.hostId===host.id);
      const top=treeGroup('host:'+host.id,host.name,hostItems,{host});
      if(host.kind==='remote'&&!hostItems.length){
        top.children.append(el('div','empty-group host-note '+host.status,hostNote(host)));
        area.append(top.group);continue;
      }
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
        if(host.kind==='remote'&&!items.length)continue;
        const child=treeGroup(`project:${project.id||'ungrouped'}:host:${host.id}`,host.name,items,{host,projectId:project.id,level:1});
        addSessions(child.children,items);top.children.append(child.group);
      }
      area.append(top.group);
    }
  }
  area.scrollTop=scroll;updateProjectControls();
}
async function refreshSessions(){
  const [list,catalog]=await Promise.all([api('/api/sessions?archived=all'),api('/api/catalog')]);
  sessions=list;projects=catalog.projects;locations=catalog.locations||[];hosts=catalog.hosts;
  if(terminalHost&&!hosts.some(host=>host.id===terminalHost.id)){terminalHost=null;if(document.body.dataset.workspaceView==='host-terminal')workspace.showView('sessions');}
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
  const saved=project?.defaultLocationId&&locations.find(l=>l.id===project.defaultLocationId);
  projectDirectory.open(saved||(!project&&$('create-dialog').open?{hostId:$('create-host').value||'local',path:''}:null));
  policyEditor.open(editingProject);$('project-dialog').showModal();$('project-name').focus();
}
function resetTranscript(){eventSeq=null;backgroundJobs.reset();subagents.reset();transcript=[];tools=new Map();approvals=new Map();assistant=null;nodes.clear();turnEnds=new Map();workLogs?.reset();$('messages').replaceChildren();$('approval-area').replaceChildren();}
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
  if(active?.id===s.id){$('welcome').hidden=true;$('conversation').hidden=false;workspace?.sessionSelected(s);sidePanel?.sessionChanged();sessionTerminal?.sessionChanged();return;}
  if(active){drafts[active.id]=$('prompt').value;sessionStorage.setItem('agentworks-drafts',JSON.stringify(drafts));scrollPositions.set(active.id,$('messages').scrollTop);}
  $('prompt').value=drafts[s.id]||'';restoreScroll=scrollPositions.get(s.id)??null;
  clearTimeout(retryTimer);generation++;socket?.close();socket=null;connected=false;
  active=s;storedEvents=[];older={mode:'none',loading:false,error:''};currentUserSeq=null;promptRecovery.render();status=s.status;modelConfig=null;permissionModes=null;sessionConfig=null;availableCommands=[];commandsOpen=false;renderCommands();mentions.close();composerMenu.close();sessionMenu.close();resetTranscript();usageStatus.reset();inputQueue.reset();
  localStorage.setItem('agentworks-session',s.id);$('welcome').hidden=true;$('conversation').hidden=false;
  setTitle(s.title);$('provider-label').textContent=labels[s.provider];$('project-path').textContent=s.cwd;
  $('session-notice').hidden=true;document.body.classList.remove('menu-open');sidebarPin.close();sidebar();updateControls();connect(generation);
  workspace?.sessionSelected(s);sidePanel?.sessionChanged();sessionTerminal?.sessionChanged();promptResize.size();
  if(s.status==='suspended'&&!s.explicitlySuspended)resumeSession(s.id);
}
function connect(gen){
  if(gen!==generation||!active)return;
  $('connection').textContent='Connecting';
  const ws=new WebSocket(socketUrl(`/api/sessions/${active.id}/events${eventSeq!==null?'?after='+eventSeq:''}`));socket=ws;
  ws.onopen=()=>{if(gen!==generation)return;connected=true;reconnectDelay=500;$('connection').textContent='Connected';updateControls();};
  ws.onmessage=event=>{
    if(gen!==generation)return;
    const e=JSON.parse(event.data);
    if(e.kind==='input_queue'){inputQueue.set(e.data);updateControls();return;}
    if(e.kind==='tool_progress'){if(applyToolProgress(tools.get(e.data.toolCallId),e.data,e.seq))scheduleDraw();return;}
    if(e.kind==='tool_header'){
      const d=e.data,tool=typeof d.parentSessionId==='string'?subagents.tool(d.acpSessionId,d.toolCallId):tools.get(d.toolCallId);
      if(tool){tool.header=d.header;scheduleDraw();}return;
    }
    if(Number.isSafeInteger(e.seq)&&e.kind!=='snapshot')eventSeq=Math.max(eventSeq??0,e.seq);
    if(e.kind==='snapshot'){
      // A full snapshot replaces the transcript; a resumed one adds only newer events.
      if(e.resumed===null||e.resumed===undefined){
        resetTranscript();usageStatus.reset();
        currentUserSeq=e.session.lastUserSeq??null;status=e.session.status;Object.assign(active,e.session);
        // Keep loaded earlier pages when they meet the current retained window without a gap.
        const first=e.events[0]?.seq,last=storedEvents.at(-1)?.seq;
        const kept=Number.isSafeInteger(first)&&Number.isSafeInteger(last)&&last>=first-1?storedEvents.filter(item=>item.seq<first):[];
        storedEvents=[...kept,...e.events];if(!kept.length)older={mode:e.truncated?'more':'none',loading:false,error:''};
        replay(e.session.lastUserSeq);
      }else{
        eventSeq=e.seq;currentUserSeq=e.session.lastUserSeq??null;status=e.session.status;Object.assign(active,e.session);
        const last=storedEvents.at(-1)?.seq??0;
        storedEvents.push(...e.events.filter(item=>Number.isSafeInteger(item.seq)&&item.seq>last));
        replaying=true;try{const latestUser=e.events.findLast(item=>item.kind==='user');for(const item of e.events)if(item.kind!=='session_updated'&&(item.kind!=='stop_requested'||item.data.userSeq===(e.session.lastUserSeq??latestUser?.seq)||(!e.session.lastUserSeq&&!latestUser)))consume(item);}finally{replaying=false;}
      }
      if(Number.isSafeInteger(e.seq))eventSeq=e.seq;
      currentUserSeq=e.session.lastUserSeq??currentUserSeq;
      backgroundJobs.set(e.asyncTasks);
      if(e.toolHeaders){
        for(const [id,tool] of tools)tool.header=e.toolHeaders[id]??null;
        subagents.setHeaders(e.toolHeaders);
      }
      for(const [id,view] of Object.entries(e.toolProgress||{}))applyToolProgress(tools.get(id),view,view.seq);
      Object.assign(active,e.session);setTitle(active.title);
      active.continuation=e.session.continuation;
      approvals=new Map(e.approvals.map(a=>[a.id,a]));status=e.session.status;modelConfig=e.models;syncModelLabel();permissionModes=e.permissionModes;sessionConfig=e.sessionConfig;active.providerCapabilities=e.providerCapabilities;attachments.setPolicy(e.attachments);if(status==='suspended'&&!e.session.explicitlySuspended&&!active.providerHistoryDeletion)resumeSession(active.id);availableCommands=e.commands||[];renderCommands();inputQueue.set(e.session.inputQueue);inputQueue.observe(status,{live:false});
      if(active.providerHistoryDeletion){$('session-notice').textContent=active.providerHistoryDeletion==='deleted'?'Provider history was deleted. This Agentworks transcript remains because local cleanup did not finish; retry Delete everywhere or delete this transcript.':'Provider-history deletion was not confirmed. This transcript is retained and resume is blocked; inspect provider history, then retry Delete everywhere or delete only this transcript.';$('session-notice').hidden=false;}
      if(e.storageError){$('session-notice').textContent=e.storageError;$('session-notice').hidden=false;}
    }else{
      if(Number.isSafeInteger(e.seq)&&e.kind!=='storage_error'&&!(e.seq<=storedEvents.at(-1)?.seq))storedEvents.push(e);
      consume(e,true);
    }
    scheduleDraw();updateControls();renderContinuation();usageStatus.render();
  };
  ws.onclose=()=>{
    if(gen!==generation)return;
    connected=false;$('connection').textContent='Reconnecting';updateControls();
    const retry=async()=>{
      if(gen!==generation||!active)return;
      try{await api(`/api/sessions/${active.id}?view=meta`);connect(gen);}catch(error){
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
// Items get ids derived from their source event so a re-fold maps them onto the same DOM nodes.
function itemId(){return itemKey?`${itemKey}:${itemCount++}`:uuid();}
function messageTime(at){const now=new Date(),clock=at.toLocaleTimeString([],{hour:'numeric',minute:'2-digit'});return at.toDateString()===now.toDateString()?clock:`${at.toLocaleDateString([],{month:'short',day:'numeric',...(at.getFullYear()===now.getFullYear()?{}:{year:'numeric'})})}, ${clock}`;}
function addMessage(role,text){const message={id:itemId(),type:'message',role,text,time:eventTime};transcript.push(message);return message;}
function replay(lastUserSeq){
  replaying=true;try{const latestUser=storedEvents.findLast(item=>item.kind==='user');for(const item of storedEvents)if(item.kind!=='session_updated'&&(item.kind!=='stop_requested'||item.data.userSeq===(lastUserSeq??latestUser?.seq)||(!lastUserSeq&&!latestUser)))consume(item);}finally{replaying=false;}
}
async function loadOlder(){
  const state=older,sid=active?.id,before=storedEvents[0]?.seq;
  if(!sid||state.loading||state.mode!=='more'||!Number.isSafeInteger(before))return;
  state.loading=true;state.error='';renderHistoryControl();
  try{
    const page=await api(`/api/sessions/${encodeURIComponent(sid)}/history?before=${before}`);
    // A snapshot that replaced the history or a session switch makes this page stale.
    if(state!==older||storedEvents[0]?.seq!==before)return;
    const events=page.events.filter(item=>Number.isSafeInteger(item.seq)&&item.seq<before);
    storedEvents=[...events,...storedEvents];state.mode=page.hasMore&&events.length?'more':'begin';
    rebuildTranscript();
  }catch(error){state.error=error.message||'Request failed';}
  finally{state.loading=false;if(state===older)renderHistoryControl();}
}
function rebuildTranscript(){
  const area=$('messages'),top=area.getBoundingClientRect().top,fromBottom=area.scrollHeight-area.scrollTop;
  // The first visible item anchors the reader's place; its node is reused below.
  let anchor=null;
  for(const item of transcript){const rect=nodes.get(item.id)?.getBoundingClientRect();if(rect?.height&&rect.bottom>top){anchor={id:item.id,offset:rect.top-top};break;}}
  const keep={status,approvals,currentUserSeq,modelConfig,permissionModes,availableCommands},oldTools=tools,oldNodes=new Map(nodes);
  const savedJobs=backgroundJobs.snapshot();
  const appliedSeq=eventSeq;resetTranscript();eventSeq=appliedSeq;rebuilding=true;
  try{replay(currentUserSeq);}finally{rebuilding=false;({status,approvals,currentUserSeq,modelConfig,permissionModes,availableCommands}=keep);}
  backgroundJobs.set(savedJobs);
  for(const tool of tools.values()){
    // Browser-observed timing and derived progress are not in stored events.
    const was=oldTools.get(tool.id);if(!was)continue;
    for(const key of ['observedStart','observedEnd','progressSeq','serverProgress'])if(was[key]!==undefined)tool[key]=was[key];
    if(tool.activityOutput&&was.activityOutput)for(const key of ['turnEndedAt','exitObservedAt'])if(was.activityOutput[key]!==undefined)tool.activityOutput[key]=was.activityOutput[key];
  }
  for(const item of transcript){const node=oldNodes.get(item.id);if(node)nodes.set(item.id,node);}
  draw();
  const node=anchor&&nodes.get(anchor.id);
  const target=node?.isConnected&&node.getBoundingClientRect().height?area.scrollTop+node.getBoundingClientRect().top-top-anchor.offset:area.scrollHeight-fromBottom;
  area.scrollTo({top:target,behavior:'instant'});
}
const historyControl=el('div','history-boundary');historyControl.setAttribute('aria-live','polite');
function renderHistoryControl(){
  if(older.mode==='begin'){historyControl.replaceChildren(el('span','history-start','Beginning of conversation'));return;}
  const button=el('button','history-more',older.loading?'Loading earlier messages…':'Load earlier messages');button.type='button';button.disabled=older.loading;button.onclick=loadOlder;
  historyControl.replaceChildren(button);if(older.error)historyControl.append(el('span','history-error','Could not load earlier messages: '+older.error));
}
// U-05 tool activity. Parser scripts run on the server (E-01) and publish an
// unsequenced derived view; the browser keeps exact raw output and observed timing.
// Keep this inline: adding a browser asset would require a coordinator-owned route change.
function applyToolProgress(tool,view,seq){
  // Views carry their source event's seq; an older view never replaces a newer one.
  if(!tool||!view||!Number.isSafeInteger(seq))return false;
  if(Number.isSafeInteger(tool.progressSeq)&&seq<tool.progressSeq)return false;
  tool.progressSeq=seq;tool.serverProgress=view;return true;
}
function toolOutputText(content){
  if(typeof content==='string')return content;
  if(!Array.isArray(content))return null;
  const texts=content.filter(c=>c?.type==='content'&&c.content?.type==='text').map(c=>c.content.text).filter(t=>typeof t==='string');
  return texts.length?texts.join('\n'):null;
}
function applyToolUpdate(tool,update,seq,observedAt=null){
  // Updates arriving while the stored body loads are applied again on top of it.
  tool.bodyQueue?.push([update,seq]);
  const output=tool.activityOutput??={streams:new Map(),version:0,seq:null};
  // Persisted session sequence numbers deduplicate replay/delivery, never text equality:
  // two identical chunks can be real output. Session snapshots rebuild all derived state.
  if(Number.isSafeInteger(seq)&&output.seq!==null&&seq<=output.seq)return false;
  if(Number.isSafeInteger(seq))output.seq=seq;
  Object.assign(tool,update,{type:'tool'});
  function write(key,data,delta){
    if(typeof data!=='string')return;
    let stream=output.streams.get(key);
    if(!delta||!stream){stream={raw:''};output.streams.set(key,stream);}
    stream.raw+=data;stream.version=++output.version;
  }
  // ACP content/rawOutput updates are snapshots. Only explicit terminal_output_delta appends.
  if(Object.hasOwn(update,'content')){
    output.streams.delete('content');const text=toolOutputText(update.content);if(text!==null)write('content',text,false);
  }
  if(Object.hasOwn(update,'rawOutput')){
    output.streams.delete('rawOutput');
    const raw=update.rawOutput;
    const text=typeof raw==='string'?raw:raw&&typeof raw==='object'?['stdout','stderr','output'].filter(k=>typeof raw[k]==='string').map(k=>raw[k]).join('\n'):null;
    if(text!==null)write('rawOutput',text,false);
  }
  const meta=update._meta;
  for(const [key,delta]of [['terminal_output',false],['terminal_output_delta',true]]){
    const value=meta?.[key];
    if(typeof value?.terminal_id==='string')write('terminal:'+value.terminal_id,value.data,delta);
  }
  const exit=meta?.terminal_exit;
  if(typeof exit?.terminal_id==='string'){
    output.exit={...exit};if(observedAt!==null)output.exitObservedAt??=observedAt;
    const stream=output.streams.get('terminal:'+exit.terminal_id);if(stream)stream.exit={...exit};
  }
  return true;
}
function toolActivityView(tool,now=Date.now(),sessionBusy=true){
  const output=tool.activityOutput;
  const server=tool.serverProgress,progress=server?.progress;
  const exit=output?.exit;
  const stopped=!!exit||!['pending','in_progress'].includes(tool.status||'pending');
  const unresolved=!stopped&&(!sessionBusy||output?.turnEnded);
  const running=!stopped&&!unresolved;
  const elapsed=tool.observedStart!==undefined?Math.max(0,Math.floor(((tool.observedEnd??output?.exitObservedAt??output?.turnEndedAt??now)-tool.observedStart)/1000)):null;
  const elapsedLabel=elapsed===null?'':elapsed<60?`${elapsed}s`:`${Math.floor(elapsed/60)}m ${elapsed%60}s`;
  const parts=[];
  if(progress){
    parts.push(server.parser+' · '+progress.phase);
    for(const c of progress.counts)parts.push(`${c.done.toLocaleString('en-US')} / ${c.total.toLocaleString('en-US')} ${c.unit} reported`);
  }else if(running)parts.push('In progress');
  else if(unresolved)parts.push('No final status');
  if(progress&&unresolved)parts.push('No final status');
  // A failed parser only removes derived progress; the tool's own status is unchanged.
  if(server?.parserFailure)parts.push('Progress unavailable');
  if(elapsedLabel)parts.push(elapsedLabel);
  if(exit){
    if(exit.exit_code!==null&&exit.exit_code!==undefined)parts.push('Exit '+exit.exit_code);
    if(exit.signal)parts.push('Signal '+exit.signal);
  }
  return {text:parts.join(' · '),running,progress:!!progress,failure:server?.parserFailure??null};
}
// Snapshots omit tool output (`bodyOmitted`); an explicit button loads stored events.
async function loadToolBody(tool){
  if(!active||!tool.bodyOmitted||tool.bodyQueue||!tool.toolCallId)return;
  const sid=active.id,gen=generation;tool.bodyQueue=[];tool.bodyError=null;
  scheduleDraw();
  try{
    const query=tool.acpSessionId?'?acpSession='+encodeURIComponent(tool.acpSessionId):'';
    const {events}=await api(`/api/sessions/${sid}/tools/${encodeURIComponent(tool.toolCallId)}${query}`);
    if(gen!==generation)return;
    const previous=tool.activityOutput,queued=tool.bodyQueue;tool.bodyQueue=null;
    // Rebuild output from the stored events; browser-observed timing is kept.
    tool.activityOutput={streams:new Map(),version:(previous?.version||0)+1,seq:null,exitObservedAt:previous?.exitObservedAt,turnEnded:previous?.turnEnded,turnEndedAt:previous?.turnEndedAt};
    for(const e of events)applyToolUpdate(tool,e.data,e.seq);
    tool.bodyOmitted=false;
    for(const [update,seq] of queued)applyToolUpdate(tool,update,seq);
  }catch(error){tool.bodyError=error.message||'Could not load tool output';}
  finally{tool.bodyQueue=null;scheduleDraw();}
}
function toolRawText(tool){
  if(tool.bodyOmitted)return tool.bodyError?'Could not load tool output: '+tool.bodyError:'Loading tool output…';
  const parts=[];
  // Preserve mixed content and raw result fields, including unrecognized metadata.
  for(const key of ['content','rawOutput','rawInput'])if(tool[key]!==undefined&&tool[key]!==null){
    const value=tool[key];parts.push(key+':\n'+(typeof value==='string'?value:JSON.stringify(value,null,2)));
  }
  if(tool._meta)parts.push('_meta:\n'+JSON.stringify(tool._meta,null,2));
  for(const [key,stream]of tool.activityOutput?.streams||[])if(key.startsWith('terminal:'))parts.push(key+' (raw output):\n'+stream.raw);
  return parts.join('\n\n')||'{}';
}
function renderToolActivity(node,tool,sessionBusy){
  const summary=node.querySelector('summary');
  // Header hooks shorten long commands; the full title stays in the tooltip and details.
  const title=el('span','tool-title',tool.header||tool.title||'Tool activity');if(tool.header&&tool.title)title.title=tool.title;
  const job=backgroundJobs.forTool(tool);
  const backgrounded=tool._meta?.jetbrains?.air?.asyncTasks?.backgrounded;
  summary.replaceChildren(title,el('span','tool-state',job?`Background · ${backgroundJobs.label(job.state)}`:backgrounded?'Background job':tool.status||'pending'));
  let loader=node.querySelector('.tool-output-loader');
  if(!loader){
    loader=el('div','tool-output-loader');
    const button=el('button','tool-output-load','Load tool output');button.type='button';
    const message=el('span','tool-output-status');message.setAttribute('role','status');
    loader.append(button,message);node.insertBefore(loader,node.querySelector('pre'));
  }
  loader.hidden=!tool.bodyOmitted;
  const button=loader.querySelector('button'),message=loader.querySelector('.tool-output-status');
  button.textContent=tool.bodyQueue?'Loading tool output…':tool.bodyError?'Retry loading tool output':'Load tool output';
  button.disabled=!!tool.bodyQueue;
  button.onclick=()=>loadToolBody(tool);
  message.textContent=tool.bodyError?'Could not load tool output: '+tool.bodyError:'';
  let activity=node.querySelector('.tool-activity');
  if(!activity){activity=el('span','tool-activity');summary.append(activity);}else summary.append(activity);
  const view=toolActivityView(tool,Date.now(),sessionBusy);
  activity.textContent=view.text;activity.hidden=!view.text;activity.title=view.failure||'';
  activity.classList.toggle('is-running',view.running);activity.classList.toggle('has-progress',view.progress);
  const output=node.querySelector('pre');output.hidden=!!tool.bodyOmitted;
  output.textContent=tool.bodyOmitted?'':toolRawText(tool);
}
// End U-05 tool activity.
// A background job's outcome, at the point in the conversation where it ended.
function renderJobResult(node,item){
  const job=item.job,tool=job.toolCallId?tools.get(job.toolCallId):null;
  const summary=node.querySelector('summary'),body=node.querySelector('.job-result-body');
  summary.replaceChildren(el('span','tool-title',job.name||'Background job'),el('span','tool-state','Background · '+backgroundJobs.label(job.state)));
  node.classList.toggle('is-failed',job.state==='failed');
  const lines=jobResultLines(job);
  // Codex reports the command's exit code on the launching tool call's raw item.
  const exit=tool?.rawOutput?.exitCode;if(Number.isInteger(exit))lines.push('Exit '+exit);
  if(!lines.length)lines.push('The adapter reported no further details for this job.');
  const text=lines.join('\n');
  if(body.dataset.text!==text){body.dataset.text=text;body.querySelector('.job-result-text').textContent=text;}
  const show=body.querySelector('button');show.hidden=!tool;
  show.onclick=()=>{
    const target=nodes.get(tool.id);if(!target)return;
    const log=target.closest('details.work-log');if(log)log.open=true;
    target.open=true;target.scrollIntoView({block:'center'});target.querySelector('summary')?.focus();
  };
}

function consume(e,live=false){
  const d=e.data;itemKey=Number.isSafeInteger(e.seq)?`${active.id}:${e.seq}`:null;itemCount=0;eventTime=Number.isSafeInteger(e.time)?e.time:null;
  if(e.kind==='user')currentUserSeq=e.seq;
  // A re-fold only rebuilds the transcript; session state, drafts and usage are already current.
  if(!rebuilding){
    promptRecovery.consume(active.id,e);
    if(e.kind==='stop_requested'&&pendingSends[active.id]?.requestId===d.requestId){delete pendingSends[active.id];sessionStorage.setItem('agentworks-pending-sends',JSON.stringify(pendingSends));}
  }
  if(e.kind==='user'||e.kind==='turn_end')for(const tool of tools.values())if(tool.activityOutput&&!tool.activityOutput.turnEnded){tool.activityOutput.turnEnded=true;if(live&&!replaying)tool.activityOutput.turnEndedAt=Date.now();}
  if(!rebuilding)usageStatus.consume(e,{live});
  backgroundJobs.consume(e);
  if(subagents.consume(e,{live:live&&!replaying}))return;
  if(d?.continuationState&&active&&!rebuilding)active.continuation=d.continuationState;
  {const t=timeoutControls?.notice(e.kind,d);if(t){transcript.push({id:itemId(),type:'notice',text:t});assistant=null;}}
  if(rebuilding&&['session_updated','continuation','models','permission_modes','attachment_policy','commands','status','storage_error'].includes(e.kind)){}
  else if(e.kind==='session_updated'){
    Object.assign(active,d);sessionTerminal?.sessionUpdated();setTitle(active.title);const row=sessions.find(s=>s.id===active.id);if(row)Object.assign(row,d);sidebar();
  }
  else if(e.kind==='continuation'){renderContinuation();}
  else if(e.kind==='models'){modelConfig=d;syncModelLabel();}
  else if(e.kind==='permission_modes'){permissionModes=d;}
  else if(e.kind==='session_config'){sessionConfig=d;}
  else if(e.kind==='provider_capabilities'){active.providerCapabilities=d;}
  else if(e.kind==='provider_history_delete_requested'){active.providerHistoryDeletion='pending';$('session-notice').textContent='Deleting provider history. This transcript is retained until deletion is confirmed.';$('session-notice').hidden=false;}
  else if(e.kind==='provider_history_deleted'){active.providerHistoryDeletion='deleted';}
  else if(e.kind==='provider_history_delete_failed'){$('session-notice').textContent='Provider-history deletion was not confirmed. The Agentworks transcript is retained. Inspect provider history, then retry Delete everywhere or delete only this transcript.';$('session-notice').hidden=false;}
  else if(e.kind==='attachment_policy'){attachments.setPolicy(d);}
  else if(e.kind==='commands'){availableCommands=Array.isArray(d)?d:[];renderCommands();}
  else if(e.kind==='user'){assistant=null;if(d.inboxDelivery)transcript.push({id:itemId(),type:'notice',text:'Automatic inbox turn authorized by you; peer messages are untrusted context.'});else if(d.initiatedBy)transcript.push({id:itemId(),type:'notice',text:d.initiatedBy.startsWith('controller:')?'Turn started by external controller '+(d.initiatorTitle||d.initiatedBy):'Turn started by agent session '+(sessions.find(s=>s.id===d.initiatedBy)?.title||d.initiatedBy)});const message=addMessage('user',d.text);message.seq=e.seq;message.inboxDelivery=!!d.inboxDelivery;if(d.attachments?.length)message.attachments=d.attachments;}
  else if(e.kind==='status'){
    status=d.status;if(!replaying)inputQueue.observe(status,{live});if(active){active.status=status;if(d.explicitlySuspended!==undefined)active.explicitlySuspended=d.explicitlySuspended;const s=sessions.find(s=>s.id===active.id);if(s)s.status=status;}sidebar();
  }else if(e.kind==='approval'){approvals.set(d.id,d);}
  else if(e.kind==='approval_resolved'){
    const pending=approvals.get(d.id);approvals.delete(d.id);
    if(pending?.kind==='question'){
      // L-02: record the outcome in the transcript; drafts end with the question.
      questions.forget(d.id);assistant=null;
      const asked=(pending.request?.message||'Question').slice(0,120);
      transcript.push({id:itemId(),type:'notice',text:`${asked} — ${d.action==='accept'?'answer sent':d.action==='decline'?'skipped':d.action==='cancel'?'cancelled by you':d.withdrawn?'withdrawn by the agent':'cancelled because the turn ended'}.`});
    }
  }
  else if(e.kind==='async_task_finished'){assistant=null;transcript.push({id:itemId(),type:'job',job:{...d},seq:e.seq});}
  else if(e.kind==='interrupted'){transcript.push({id:itemId(),type:'notice',text:d.message,interrupted:true});assistant=null;}
  else if(e.kind==='storage_error'){status='error';$('session-notice').textContent=d.message;$('session-notice').hidden=false;}
  else if(e.kind==='error'){transcript.push({id:itemId(),type:'error',text:d.message});assistant=null;}
  else if(e.kind==='turn_end'){assistant=null;turnEnds.set(transcript.at(-1)?.id,{seq:e.seq,stopReason:d?.timedOut?`timeout_${d.timedOut}`:d?.stopReason});}
  else if(e.kind==='fork'){
    // L-08: the carried transcript belongs to another session; say so plainly.
    transcript.push({id:itemId(),type:'notice',
      text:`Forked from “${d.parentTitle}” after ${d.boundaryTurn===1?'its first finished turn':`turn ${d.boundaryTurn}`}${d.parentProvider?` (${labels[d.parentProvider]||d.parentProvider})`:''}. The conversation below is recorded history. Context handoff uses the chosen native or fresh-session method.`});
    assistant=null;
  }
  else if(e.kind==='fork_mode'){assistant=null;transcript.push({id:itemId(),type:'notice',text:d.mode==='native'?'Provider native fork created at the recorded boundary.':`${d.detail}${d.truncated?' Available source history is truncated.':''}`});}
  else if(e.kind==='carried_context'){assistant=null;transcript.push({id:itemId(),type:'notice',text:'Context handoff dispatched with this message; interrupted delivery requires inspection.'});}
  else if(e.kind==='acp'){
    switch(d.sessionUpdate){
      case 'user_message_chunk':{
        assistant=null;if(d.content?.type==='text'){
          const last=transcript.at(-1);
          if(last?.replayed&&last.role==='user'&&last.providerMessageId===d.messageId)last.text+=d.content.text;
          else{const message=addMessage('user',d.content.text);message.replayed=true;message.time=null;message.providerMessageId=d.messageId;message.seq=e.seq;}
        }break;
      }
      case 'agent_message_chunk':{
        if(d.content?.type==='text'){
          if(!assistant||(d.messageId&&assistant.providerMessageId&&d.messageId!==assistant.providerMessageId))assistant=addMessage('assistant','');assistant.providerMessageId=d.messageId;assistant.referenceCwd=d.forkSourceCwd;assistant.text+=d.content.text;
        }else if(d.content?.type==='image'){
          transcript.push({id:itemId(),type:'image',content:d.content});assistant=null;
        }else if(d.content?.type==='resource_link'){
          assistant=null;addMessage('assistant',`[${d.content.name||'Open file'}](${d.content.uri})`).referenceCwd=d.forkSourceCwd;
        }break;
      }
      case 'tool_call':{
        assistant=null;let tool=tools.get(d.toolCallId);if(!tool){tool={id:d.toolCallId,type:'tool'};tools.set(d.toolCallId,tool);transcript.push(tool);}if(applyToolUpdate(tool,d,e.seq,live&&!replaying?Date.now():null))observeTool(tool,{live:live&&!replaying});break;
      }
      case 'tool_call_update':{
        let tool=tools.get(d.toolCallId);
        if(!tool){tool={id:d.toolCallId,type:'tool',title:'Tool activity'};tools.set(d.toolCallId,tool);transcript.push(tool);}if(applyToolUpdate(tool,d,e.seq,live&&!replaying?Date.now():null))observeTool(tool,{live:live&&!replaying});break;
      }
      case 'plan':{
        const text=(d.entries||[]).map(p=>`- ${p.status==='completed'?'✓':'○'} ${p.content}`).join('\n');
        transcript.push({id:itemId(),type:'plan',text});break;
      }
      default:break;
    }
  }
}
async function resumeSession(id){
  try{await api(`/api/sessions/${id}/resume`,{method:'POST',body:'{}'});}catch(e){toast(e.message);}
}
// Additional settings come from the adapter, including unknown select categories.
function renderSessionConfig(){
  const root=$('session-config-fields');
  const options=(sessionConfig?.configOptions||[]).filter(o=>['select','boolean'].includes(o.type)&&o.id!==modelConfig?.configId&&o.id!==permissionModes?.configId&&!(active?.modelProvider&&o.id==='provider'));
  const signature=JSON.stringify(options);
  if(root.dataset.config!==signature){
    root.replaceChildren();root.dataset.config=signature;
    for(const option of options){
      const label=el('label','session-config-field');label.append(el('span','',option.name||option.id));
      if(option.description)label.title=option.description;
      let control;
      if(option.type==='boolean'){
        control=el('input');control.type='checkbox';control.checked=option.currentValue===true;
      }else{
        control=el('select');
        for(const choice of option.options||[]){
          if(Array.isArray(choice.options)){
            const group=el('optgroup');group.label=choice.name||choice.group;
            for(const item of choice.options){const node=el('option','',item.name||item.value);node.value=item.value;group.append(node);}control.append(group);
          }else{const node=el('option','',choice.name||choice.value);node.value=choice.value;control.append(node);}
        }
        control.value=option.currentValue;
      }
      control.setAttribute('aria-label',option.name||option.id);
      control.onchange=async()=>{
        const id=active?.id;if(!id)return;
        control.disabled=true;
        try{await api(`/api/sessions/${id}/configuration`,{method:'POST',body:JSON.stringify({configId:option.id,value:option.type==='boolean'?control.checked:control.value})});}
        catch(error){toast(error.message);delete root.dataset.config;updateControls();}
      };
      label.append(control);root.append(label);
    }
  }
  $('session-config-options').hidden=!options.length;
  for(const control of root.querySelectorAll('select,input'))control.disabled=!connected||status!=='idle';
}

function updateControls(){
  backgroundJobs.render();questions.refresh();
  $('archive-session').textContent=active?.archived?'Unarchive session':'Archive session';
  $('suspend-session').disabled=!connected||status==='suspended'||status==='closed';
  $('resume-session').hidden=!['suspended','error'].includes(status);
  $('resume-session').disabled=!connected||!!active?.providerHistoryDeletion;
  $('show-commands').disabled=!connected||status!=='idle';
  const usage=usageCommand();
  $('show-usage').hidden=!['claude','claude-rust','codex','codex-rust'].includes(active?.provider)||!usage;
  $('show-usage').disabled=!connected||status!=='idle';
  $('delete-provider-history').hidden=!active?.providerCapabilities?.delete;
  renderSessionConfig();
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
  $('prompt').disabled=!connected||!(status==='idle'||busy);$('send').disabled=!connected||status==='cancelling'||!(status==='idle'||busy);
  $('send').setAttribute('aria-label',busy?'Queue message':'Send message');$('send').textContent=busy?'＋':'↑';$('send').title=busy?'Queue to send after this turn ends':'';
  inputQueue.render();inputQueue.renderActivity();$('stop').hidden=!busy&&!['starting','resuming','configuring'].includes(status)&&!active?.continuation?.enabled;$('stop').disabled=!connected||status==='cancelling';
  attachments.render();
  $('prompt').placeholder=status==='starting'?'Starting your agent…':status==='waiting'?'Respond to the permission request above…':busy?'Your agent is working… type to queue a follow-up':status==='error'?'Session unavailable. Use Resume session to try again.':'What would you like to work on?';
  if(subagents.viewingChild())$('prompt').placeholder=busy?'Queue a message for the main agent…':'Message the main agent…';
}
function scheduleDraw(){if(!drawTimer)drawTimer=setTimeout(()=>{drawTimer=null;draw();},70);}
function fileUrl(path,download=false){return appUrl(`/api/sessions/${active.id}/file?path=${encodeURIComponent(path)}${download?'&download=true':''}`);}
const imageViewer=setupImageViewer($('messages'));
const attachments=setupAttachments({appUrl,el,toast,uuid,getSession:()=>active,canAttach:()=>!!active&&!$('prompt').disabled,button:$('attach'),input:$('attach-input'),list:$('attachment-list'),dropTarget:$('composer')});
const decorateMarkdown=setupChatReferences({getSession:()=>active,getSessions:()=>sessions,openSession:async id=>{
  try{const target=sessions.find(s=>s.id===id)||(await api('/api/sessions/'+encodeURIComponent(id))).session;selectSession(target);}
  catch(e){toast('Cannot open session: '+e.message);}
},openFile:(session,path,line)=>workspace?.openSideFile(session,path,line),fileUrl,imageViewer});
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
      message.html=result.html;body.innerHTML=result.html;body.classList.add('markdown');decorateMarkdown(body,message.referenceCwd?message.referenceCwd.replace(/\/$/,'')+'/':'');enhanceMermaid(body,result.mermaidClass);display.enhance(body,message.id);
    }catch{body.textContent=message.text;}finally{
      message.markdownPending=false;
      if(text!==message.text){delete body.dataset.source;updateMarkdown(message,body);}
    }
  },status==='idle'?0:250);
}
function draw(){
  if(!active)return;
  backgroundJobs.render();
  const area=$('messages'),visible=!area.hidden&&!$('session-chat').hidden;const atBottom=visible&&area.scrollHeight-area.scrollTop-area.clientHeight<100;
  const empty=area.querySelector('.session-empty');if(transcript.length)empty?.remove();
  if(older.mode==='none')historyControl.remove();else if(area.firstChild!==historyControl){renderHistoryControl();area.prepend(historyControl);}
  if(!transcript.length&&!empty){
    const wrap=el('div','session-empty');wrap.append(el('div','empty-symbol',icons[active.provider]),el('h3','',status==='starting'?'Preparing your workspace':'Ready when you are'),el('p','',active.provider==='demo'?'Try “hello”, “approval”, or “slow”. This demo uses no API credits.':'Send a message to start working in this project.'));area.append(wrap);
  }
  for(const item of transcript){
    let node=nodes.get(item.id);
    if(!node){
      if(item.type==='message'){
        node=el('article','message '+(item.inboxDelivery?'agentworks':item.role));node.dataset.messageId=item.id;node.tabIndex=0;node.append(el('div','avatar',item.inboxDelivery?'A':item.role==='user'?'Y':icons[active.provider]));
        const content=el('div','message-content'),label=el('div','message-label',item.inboxDelivery?'Agentworks':item.role==='user'?'You':labels[active.provider]);
        // Event time, not provider time; older stored events carry none.
        if(item.time){const at=new Date(item.time),time=el('time','message-time',messageTime(at));time.dateTime=at.toISOString();time.title=at.toLocaleString();label.append(time);}
        content.append(label,el('div','message-body'));node.append(content);
      }else if(item.type==='tool'){
        node=el('details','tool-card');node.append(el('summary'),el('pre'));
      }else if(item.type==='job'){
        node=el('details','tool-card job-result');const body=el('div','job-result-body'),show=el('button','text-button','Show launching tool call');show.type='button';
        body.append(el('div','job-result-text'),show);node.append(el('summary'),body);
      }else if(item.type==='image'){
        node=el('div','message');const image=el('img','agent-image');
        if(/^image\/(png|jpeg|gif|webp)$/.test(item.content.mimeType)&&item.content.data.length<14*1024*1024){image.src=`data:${item.content.mimeType};base64,${item.content.data}`;image.alt='Image from agent';imageViewer.decorate(image);node.append(image);}else node.textContent='Unsupported image format';
      }else node=el('div',item.type==='error'?'error-card':'notice-card',item.text);
      nodes.set(item.id,node);area.append(node);
    }else if(!node.isConnected)area.append(node);
    if(item.type==='message'){
      const body=node.querySelector('.message-body');
      if(item.role==='user'&&!item.inboxDelivery){renderUserReferences(body,item.text);decorateMarkdown(body);if(item.attachments&&!node.querySelector('.message-attachments')){const sent=el('div','message-attachments');attachments.renderSent(sent,active.id,item.attachments);for(const img of sent.querySelectorAll('img'))imageViewer.decorate(img);body.after(sent);}}else updateMarkdown(item,body);
    }else if(item.type==='tool'){
      renderToolActivity(node,item,['running','waiting','cancelling'].includes(status));
    }else if(item.type==='job')renderJobResult(node,item);
  }
  subagents.sync(['running','waiting','cancelling'].includes(status));
  const pending=$('approval-area');for(const node of [...pending.children])if(!node.classList.contains('question'))node.remove();
  workLogs.apply(area,transcript,nodes,{sessionIdle:['idle','closed','suspended','error'].includes(status),scope:active.id,turnEnds});
  questions.sync(pending,[...approvals].filter(([,a])=>a.kind==='question'));
  for(const [id,{kind,request}]of approvals){
    if(kind==='question')continue;
    const card=el('div','approval');card.append(el('h4','',subagents.name(request.sessionId)?`Permission requested · ${subagents.name(request.sessionId)}`:'Permission requested'),el('p','',request.toolCall?.title||'An agent needs your decision'));
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
  if(restoreScroll!==null&&visible){area.scrollTop=restoreScroll;restoreScroll=null;}else if(atBottom)area.scrollTop=area.scrollHeight;
}
$('new-session').onclick=()=>openCreate();$('welcome-new').onclick=()=>openCreate();
$('view-hosts').onclick=()=>setSidebarView('hosts');$('view-projects').onclick=()=>setSidebarView('projects');
$('new-project').onclick=()=>sidebarView==='hosts'?openHostDialog():openProjectDialog();$('create-project-new').onclick=()=>openProjectDialog();
let editingHostRoots=null;
function openHostRoots(host){
  editingHostRoots=host.id;$('host-roots-title').textContent='Settings for '+host.name;
  const tools=$('host-tools');tools.replaceChildren();
  tools.append(el('p','field-note',`Tools and sign-ins belong to ${host.destination}. Sign in over SSH as that user, then retry the session.`));
  for(const provider of ['codex','claude','opencode']){
    const path=host.worker?.tools?.[provider];
    const row=el('div','field-note');row.append(el('strong','',labels[provider]+': '),el('code','',path||(host.worker?.tools?'Not found by the worker':host.worker?'Update this worker to discover CLI paths':'Connect this host to discover CLI paths')));
    if(path){const command=provider==='codex'?' login':' auth login';row.append(el('div','',`On ${host.destination}, run: `),el('code','',`'${path.replaceAll("'", "'\\''")}'${command}`));}
    tools.append(row);
  }
  $('host-roots').value=(host.roots||[]).join('\n');$('host-roots-error').textContent='';$('host-roots-dialog').showModal();$('host-roots').focus();
}
$('dismiss-host-roots').onclick=()=>$('host-roots-dialog').close();
$('host-roots-form').onsubmit=async event=>{
  event.preventDefault();$('host-roots-error').textContent='';$('save-host-roots').disabled=true;
  try{
    const roots=$('host-roots').value.split('\n').map(r=>r.trim()).filter(Boolean);
    await api(`/api/hosts/${editingHostRoots}/roots`,{method:'PUT',body:JSON.stringify({roots})});
    $('host-roots-dialog').close();await refreshSessions();
  }catch(error){$('host-roots-error').textContent=error.message;}finally{$('save-host-roots').disabled=false;}
};
const enrollment=hostEnrollment({$,api,refresh:refreshSessions,directories:host=>{
  $('host-dialog').close();openHostRoots(host);
}});
function openHostDialog(){enrollment.open();}
$('dismiss-project').onclick=()=>{if(!savingProject)$('project-dialog').close();};
$('project-dialog').addEventListener('cancel',event=>{if(savingProject)event.preventDefault();});
$('create-project').onchange=()=>{const remembered=projectDirectoryOn($('create-host').value||'local',$('create-project').value||null)||rememberedDirectory();if(remembered)$('directory').value=remembered;worktrees.projectChanged();updateCommunicationDefault();};
$('project-form').onsubmit=async event=>{
  event.preventDefault();if(savingProject)return;savingProject=true;
  for(const id of ['save-project','project-name','project-directory','project-directory-host','project-directory-browse','project-directory-clear','project-default-mode','project-require-worktree','project-communication-role','archive-project','delete-project','dismiss-project'])$(id).disabled=true;
  $('project-error').textContent='';
  try{
    const policy=policyEditor.value(),directory=projectDirectory.value();
    const project=await api(editingProject?`/api/projects/${editingProject}`:'/api/projects',{method:editingProject?'PATCH':'POST',body:JSON.stringify({name:$('project-name').value,defaultCommunicationRole:$('project-communication-role').value||null,...(directory!==undefined?{defaultLocation:directory}:{})})});
    editingProject=project.id;projectDirectory.commit();$('delete-project').hidden=false;$('archive-project').hidden=false;
    try{await api(`/api/projects/${project.id}/workspace-policy`,{method:'PUT',body:JSON.stringify(policy)});}
    catch(error){throw new Error(`Project name saved. Workspace policy was not saved: ${error.message}. Retry Save project.`);}
    await refreshSessions();if($('create-dialog').open){$('create-project').value=project.id;const start=projectDirectoryOn($('create-host').value||'local',project.id);if(start){$('directory').value=start;$('directory').dispatchEvent(new Event('input'));$('directory').dispatchEvent(new Event('change'));}worktrees.projectChanged();updateCommunicationDefault();}$('project-dialog').close();
  }catch(error){$('project-error').textContent=error.message;}finally{savingProject=false;for(const id of ['save-project','project-name','project-directory','project-directory-host','project-directory-browse','project-directory-clear','project-default-mode','project-require-worktree','project-communication-role','archive-project','delete-project','dismiss-project'])$(id).disabled=false;}
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
    :`Hides the project and archives its ${count} ${plural}. Archiving stops session terminals and keeps their saved history.`;
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
const modelProviders=setupModelProviders({$,api,el,getHosts:()=>hosts,selectionChanged:()=>{resetCreateModels();providerNote();}});
$('provider').onchange=()=>{providerNote();resetCreateModels();modelProviders.loadCreate();updateRoleNote();};
$('create-role').onchange=()=>updateRoleNote();
$('directory').onchange=resetCreateModels;
$('load-models').onclick=loadCreateModels;
$('create-model').onchange=()=>{modelPicked=true;};
$('show-commands').onclick=()=>{commandsOpen=!commandsOpen;renderCommands();};
$('show-usage').onclick=async()=>{
  if(!active||status!=='idle')return;
  $('show-usage').disabled=true;
  try{await api(`/api/sessions/${active.id}/prompt`,{method:'POST',body:JSON.stringify({text:active.provider.startsWith('claude')?'/usage':'/status',requestId:uuid()})});}
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
    const source=$('directory').value.trim();
    const payload={resumePolicy:{enabled:$('create-resume-policy').checked},provider:$('provider').value,modelProvider:modelProviders.selection(),...worktrees.submission(),title:$('session-title').value,projectId:$('create-project').value||null,hostId:$('create-host').value,model:$('create-model').value,...(!fork&&$('create-role').value?{role:$('create-role').value}:{}),...($('create-agent-access').value?{agentAccess:{communicate:$('create-agent-access').value!=='off',...Object.fromEntries(['startSessions','promptSessions','cancelSessions','runAutomations'].map(key=>[key,$('create-agent-access').value==='coordinator']))}}:{})};
    if(fork)Object.assign(payload,{boundarySeq:$('fork-boundary').value?Number($('fork-boundary').value):null,context:$('fork-context').value,summary:$('fork-context').value==='summary'?$('fork-summary').value:null});
    const signature=JSON.stringify({source:fork?.id||null,payload});
    if(!pendingCreate||pendingCreate.signature!==signature)pendingCreate={signature,requestId:uuid()};
    sessionStorage.setItem('agentworks-pending-create',JSON.stringify(pendingCreate));
    const path=fork?`/api/sessions/${fork.id}/fork`:'/api/sessions';
    const created=await api(path,{method:'POST',body:JSON.stringify({...payload,requestId:pendingCreate.requestId})});
    pendingCreate=null;sessionStorage.removeItem('agentworks-pending-create');
    // Remember the chosen source directory, never a worktree created from it.
    const remembered=readStorage(localStorage,'agentworks-project-locations',{});remembered[created.hostId&&created.hostId!=='local'?created.hostId+':'+(created.projectId||'ungrouped'):created.projectId||'ungrouped']=source;try{localStorage.setItem('agentworks-project-locations',JSON.stringify(remembered));}catch{}
    sessions.unshift(created);$('create-dialog').close();selectSession(created);
    if(fork)toast(`Forked from “${fork.title}”. Its recorded history and lineage are preserved; the selected context handoff is applied.`,'success');
  }catch(e){$('create-error').textContent=e.message;}finally{creating=false;providerNote();}
};
// L-05: one stable pending identity per session (survives switches, reloads and lost
// responses) shared by direct send and queueing, so a retry can never send twice.
const pendingSends=readStorage(sessionStorage,'agentworks-pending-sends',{});
function pendingFor(sid,text,attached=[]){
  // Attachment IDs are part of the intended input: a different set is a new request.
  const same=pendingSends[sid]?.text===text&&JSON.stringify(pendingSends[sid].attachments||[])===JSON.stringify(attached);
  if(!same)pendingSends[sid]={text,requestId:uuid(),...(attached.length?{attachments:attached}:{})};
  sessionStorage.setItem('agentworks-pending-sends',JSON.stringify(pendingSends));return pendingSends[sid];
}
// draft is the composer text; submission.text may carry resolved @mentions.
function settle(sid,submission,draft=submission.text){
  const recovered=promptRecovery.recovered(sid,submission.requestId);
  if(!recovered&&active?.id===sid&&$('prompt').value===draft){$('prompt').value='';promptResize.size();}
  if(!recovered&&drafts[sid]===draft)delete drafts[sid];sessionStorage.setItem('agentworks-drafts',JSON.stringify(drafts));
  if(pendingSends[sid]?.requestId===submission.requestId){delete pendingSends[sid];sessionStorage.setItem('agentworks-pending-sends',JSON.stringify(pendingSends));}
}
$('composer').onsubmit=async event=>{
  event.preventDefault();const text=$('prompt').value;if(!active)return;
  const sid=active.id,attached=attachments.ids(sid);
  if(attachments.count(sid)){
    if(attachments.uploading(sid)){toast('Wait for attachments to finish uploading');return;}
    if(attachments.failed(sid)){toast('Remove attachments that failed to upload');return;}
    if(!text.trim()){toast('Add a message to send with the attachments');return;}
    // Queued inputs are text-only; attachments are never held for a later turn.
    if(BUSY.includes(status)){toast('Attachments can’t be queued. Send once the agent is idle; your draft is kept.');return;}
  }
  if(!text.trim())return;
  if(status==='cancelling'||(!BUSY.includes(status)&&status!=='idle'))return;
  const submission={...pendingFor(sid,mentions.expand(text),attached)};
  promptRecovery.submitted(sid,submission,text);
  $('send').disabled=true;
  try{
    if(BUSY.includes(status))await inputQueue.enqueue(sid,submission);
    else try{await api(`/api/sessions/${sid}/prompt`,{method:'POST',body:JSON.stringify(submission)});}
    catch(e){
      // Send→Enter race: the session became busy first. Queue the same intended input.
      if(e.status!==409||!/become idle/.test(e.message))throw e;
      if(attached.length)throw new Error('The agent became busy first. Attachments are kept; send again once it is idle.');
      await inputQueue.enqueue(sid,submission);
    }
    settle(sid,submission,text);
    if(attached.length)attachments.clear(sid,attached);
  }catch(e){toast(e.message);}finally{updateControls();}
};
$('prompt').oninput=()=>{mentions.update();commandsOpen=/^[/\$]\S*$/.test($('prompt').value);renderCommands();if(active){drafts[active.id]=$('prompt').value;sessionStorage.setItem('agentworks-drafts',JSON.stringify(drafts));}};
$('prompt').onkeydown=event=>{if(mentions.keydown(event))return;if(event.key==='Enter'&&!event.shiftKey&&!event.isComposing){event.preventDefault();$('composer').requestSubmit();}};
$('resume-session').onclick=()=>{if(active)resumeSession(active.id);};
$('stop').onclick=async()=>{if(!active)return;const sid=active.id,seq=currentUserSeq;$('stop').disabled=true;try{await api(`/api/sessions/${sid}/cancel${seq!=null?'?userSeq='+seq:''}`,{method:'POST',body:'{}'});}catch(e){toast(e.message);}finally{updateControls();}};
async function deleteSession(session,providerHistory=false){
  if(!session)return;
  const question=providerHistory
    ? `Permanently delete “${session.title}” from Agentworks and its provider’s conversation history? Its harness will stop. Worktree files and branches remain. This cannot be undone.`
    : `Permanently delete “${session.title}” and its Agentworks transcript? Its harness will stop. Provider conversation history, worktree files and branches remain. This cannot be undone.`;
  if(!confirm(question))return;
  const id=session.id;
  try{
    await api(`/api/sessions/${id}${providerHistory?'?providerHistory=true':''}`,{method:'DELETE'});
    if(active?.id===id)closeActive();
    await refreshSessions();
  }catch(e){toast(e.message);}
}
$('close-session').onclick=()=>deleteSession(active);
$('delete-provider-history').onclick=()=>deleteSession(active,true);
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
  parserScripts.load();
  notifications.load().then(()=>notifications.mount($('notification-controls'))).catch(error=>notifications.mount($('notification-controls'),error));
  try{
    modelProviders.loadSettings();
    const settings=await api('/api/settings');$('settings-roots').replaceChildren();for(const root of settings.roots)addRoot(root);$('settings-error').textContent='';$('settings-saved').textContent='Save applies global defaults and host settings. Display, notifications, model providers and parsers save immediately.';
    $('default-communication-role').value=settings.defaultCommunicationRole||'participant';
    const fields=$('default-model-fields');fields.replaceChildren();
    // Node and Rust harnesses for one CLI share its permission default; show the picker once per CLI.
    const modeFamilies=new Set();
    for(const provider of config.providers.filter(p=>p.id!=='demo')){
      const family=provider.id.replace(/-rust$/,''),firstOfFamily=!modeFamilies.has(family);modeFamilies.add(family);
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
      if(family==='codex'&&firstOfFamily){
        const modeLabel=el('label','','Codex default sandbox / permission mode');modeLabel.htmlFor='codex-default-permission-mode';
        const mode=el('select');mode.id='codex-default-permission-mode';
        for(const [value,name] of [['agent-full-access','Full access'],['read-only','Read-only'],['workspace-write','Workspace access'],['agent','Auto review']]){
          const option=el('option','',name);option.value=value;mode.append(option);
        }
        mode.value=settings.codexDefaultPermissionMode||'agent-full-access';
        const note=el('div','field-note',"Applies only to new Codex sessions, in both the Node and Rust harnesses. Full access allows unrestricted file and network access in the Codex harness. Existing sessions keep their saved mode. If the adapter does not advertise your choice, its current mode is kept and a note appears in Session options. This does not change Agentworks MCP/controller grants or host controls.");
        fields.append(modeLabel,mode,note);
      }
      if(family==='claude'&&firstOfFamily){
        const modeLabel=el('label','','Claude default permission mode');modeLabel.htmlFor='claude-default-permission-mode';
        const mode=el('select');mode.id='claude-default-permission-mode';
        for(const [value,name] of [['auto','Auto'],['default','Default'],['acceptEdits','Accept edits'],['plan','Plan']]){
          const option=el('option','',name);option.value=value;mode.append(option);
        }
        mode.value=settings.claudeDefaultPermissionMode||'auto';
        const note=el('div','field-note',"Applies only to new Claude sessions, in both the Node and Rust harnesses. Auto uses Claude’s normal Auto mode. If the adapter does not advertise your choice, its current permission mode is kept and a note appears in Session options. Reconnected sessions keep their saved mode.");
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
    const settings=await api('/api/settings',{method:'PUT',body:JSON.stringify({roots:rootValues().filter(Boolean),defaultModels,claudeDefaultPermissionMode:$('claude-default-permission-mode')?.value,codexDefaultPermissionMode:$('codex-default-permission-mode')?.value,defaultCommunicationRole:$('default-communication-role').value,...(timeouts?{timeouts}:{})})});
    if(timeouts)timeoutControls.acceptDefault(timeouts);
    config.roots=settings.roots;config.defaultModels=settings.defaultModels;config.defaultCommunicationRole=settings.defaultCommunicationRole||'participant';applyRoots();
    $('settings-dialog').close();toast('Settings saved. They apply to new sessions.','success');
  }catch(e){$('settings-error').textContent='Host settings were not saved: '+e.message+' Display preferences are stored separately in this browser.';}finally{$('save-settings').disabled=false;}
};
document.addEventListener('keydown',event=>{if(event.key==='n'&&!event.ctrlKey&&!event.metaKey&&!event.altKey&&!['INPUT','TEXTAREA','SELECT'].includes(document.activeElement?.tagName)&&!document.querySelector('dialog[open]')){event.preventDefault();openCreate();}});
const diffView=setupDiff({api,root:$('side-diff'),enhanceBlocks:display.enhance});
timeoutControls=setupTimeoutControls({$,api,getActive:()=>active});
parserScripts=setupParserScripts({$,api,el});
workspace=createWorkspace({api,toast,labels,uuid,
  viewChanged(view){
    if(view==='host-terminal'){
      terminalHost=hosts.find(host=>host.id===(terminalHost?.id||localStorage.getItem('agentworks-terminal-host')));
      if(!terminalHost){workspace.showView('sessions');return;}
      $('header-title').textContent=terminalHost.name+' · Terminal';$('host-terminal-title').textContent=terminalHost.name+' · Terminal';
      hostTerminal.open();document.body.classList.remove('menu-open');enterChat();
    }else if(active)setTitle(active.title);else $('header-title').textContent='Overview';
  },getConfig:()=>config,getActive:()=>active,selectSession,openCreate,refreshSessions,enhanceBlocks:display.enhance,openSettings,
  overview(){ $('conversation').hidden=true;$('welcome').hidden=false;localStorage.removeItem('agentworks-session'); },
  openPanel:tab=>sidePanel?.open(tab),
  appendDraft(text){if(!active)return;const current=$('prompt').value;$('prompt').value=current?current+'\n\n'+text:text;drafts[active.id]=$('prompt').value;sessionStorage.setItem('agentworks-drafts',JSON.stringify(drafts));promptResize.size();},
});
const sessionTerminal=setupTerminal({$,api,getActive:()=>active,socketUrl,onSelect:view=>{subagents.select(view);updateControls();scheduleDraw();}});
const hostTerminal=setupTerminal({$:id=>$(id==='session-terminal'?'host-terminal-view':id.replace(/^terminal-/,'host-terminal-')),api,getActive:()=>terminalHost,socketUrl,tabs:false,endpoint:id=>`/api/hosts/${id}/terminal`});
$('host-terminal-back').onclick=()=>{workspace.showView('sessions');sidebar();if(phone())showBrowser();};

sidePanel=setupSidePanel({panel:$('side-panel'),tabs:{files:$('side-tab-files'),diff:$('side-tab-diff')},triggers:{files:$('session-files'),diff:$('session-diff')},close:$('side-close'),renderers:{files:workspace.sideFiles,diff:diffView},getActive:()=>active,phone});
async function init(){
  config=await api('/api/status');
  $('host-address').textContent=location.host+' · Linux prototype';
  for(const p of config.providers){const option=el('option','',p.name+(p.authMode==='api_key'&&!p.keyPresent?' · API key needed':''));option.value=p.id;$('provider').append(option);}
  applyRoots();
  await refreshSessions();
  const saved=sessions.find(s=>s.id===localStorage.getItem('agentworks-session'));if(saved)selectSession(saved);workspace.restoreView();
  // The dashboard's Settings link opens the dialog here; drop the flag so a reload doesn't reopen it.
  const params=new URLSearchParams(location.search);if(params.has('settings')){params.delete('settings');history.replaceState(history.state,'',location.pathname+(params.size?'?'+params:'')+location.hash);openSettings();}
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
// Apple keyboards use ⌘ for the same shortcut.
if(/Mac|iPhone|iPad/.test(navigator.platform)){$('palette-shortcut').textContent='⌘K';$('palette-open').title='Command palette (⌘K)';}
function usageCommand(){return availableCommands.find(c=>c.name===(active?.provider?.startsWith('claude')?'usage':'status'));}
setupPalettes({
  $,labels,selectSession,
  fallbackFocus:()=>active?$('prompt'):null,
  state:()=>({active,status,connected,sessions,projects,hosts,commands:availableCommands,
    usageAvailable:!!active&&['claude','claude-rust','codex','codex-rust'].includes(active.provider)&&!!usageCommand(),
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

// Refresh observed elapsed activity without reparsing output or changing transcript state.
setInterval(()=>{if(!active)return;for(const tool of tools.values()){const node=nodes.get(tool.id);if(node){const view=toolActivityView(tool,Date.now(),['running','waiting','cancelling'].includes(status));const activity=node.querySelector('.tool-activity');if(activity&&view.running)activity.textContent=view.text;}}},1000);
