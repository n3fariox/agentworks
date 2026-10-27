import {setupTerminal} from './terminal.js';
// Shared provider definitions, host availability, and session selection.
export function setupModelProviders({$,api,el,getHosts,selectionChanged,socketUrl}) {
  let editing=null,profiles=[],settingsGeneration=0,createGeneration=0,busy=false;
  let auth=null,codexAuth=null,codexPoll=null,codexGeneration=0,codexDevice=false;
  const codexPath=id=>`/api/model-providers/${encodeURIComponent(id)}/codex-auth`;
  function stopCodexPoll(){clearTimeout(codexPoll);codexPoll=null;codexGeneration++;}
  async function pollCodex(id,generation){
    try{
      const result=await api(codexPath(id));if(generation!==codexGeneration)return;
      if(result.status==='pending'||result.status==='exchanging'){
        $('codex-oauth-status').textContent=result.status==='pending'?(codexDevice?'Enter the displayed code on the OpenAI page.':'Complete sign-in in the ChatGPT tab.'):'Completing sign-in…';
        codexPoll=setTimeout(()=>pollCodex(id,generation),1000);
      }else{
        $('codex-oauth-cancel').hidden=true;$('codex-oauth-link').hidden=true;$('codex-device-instructions').hidden=true;
        $('codex-oauth-status').textContent=result.error||(result.signedIn?'Signed in. Enabled hosts can use this ChatGPT account.':'Sign-in cancelled.');
        await list();await loadCreate();
      }
    }catch(error){if(generation===codexGeneration)$('codex-oauth-status').textContent=error.message;}
  }
  async function signInCodex(p,device=false){
    stopCodexPoll();const generation=codexGeneration;codexAuth=p.id;codexDevice=device;
    const popup=device?null:window.open('about:blank','_blank');if(popup)popup.opener=null;
    $('codex-oauth').hidden=false;$('codex-oauth-title').textContent=p.name+(device==='rig'?' · ChatGPT sign-in · Rig':' · ChatGPT sign-in');
    $('codex-device-instructions').hidden=true;$('codex-device-code').textContent='';$('codex-oauth-status').textContent='Starting sign-in…';$('codex-oauth-link').hidden=true;$('codex-oauth-cancel').hidden=true;
    $('codex-oauth').scrollIntoView({block:'center'});
    try{
      const result=await api(codexPath(p.id)+(device===true?'/device':device==='rig'?'/device-rig':''),{method:'POST'});if(generation!==codexGeneration){popup?.close();return;}
      if(device){$('codex-device-code').textContent=result.userCode;$('codex-device-instructions').hidden=false;}
      $('codex-oauth-link').href=result.verificationUrl||result.authorizationUrl;$('codex-oauth-link').hidden=false;$('codex-oauth-cancel').hidden=false;
      if(popup)popup.location.href=result.authorizationUrl;
      pollCodex(p.id,generation);
    }catch(error){popup?.close();$('codex-oauth-status').textContent=error.message;}
  }
  $('codex-oauth-cancel').onclick=async()=>{const id=codexAuth;stopCodexPoll();try{await api(codexPath(id)+'/cancel',{method:'POST'});$('codex-oauth-status').textContent='Sign-in cancelled.';$('codex-oauth-cancel').hidden=true;$('codex-oauth-link').hidden=true;$('codex-device-instructions').hidden=true;}catch(error){$('codex-oauth-status').textContent=error.message;}};
  addEventListener('pagehide',stopCodexPoll);
  const authPath=id=>`/api/model-providers/${encodeURIComponent(id)}/auth`;
  const authTerminal=setupTerminal({$:id=>$(id==='session-terminal'?'provider-auth-terminal':id.replace(/^terminal-/,'provider-auth-')),api,getActive:()=>auth,socketUrl,tabs:false,runningStatus:'Sign-in running',exitStatus:frame=>frame.code===0?'Sign-in completed':frame.code==null?'Sign-in stopped':'Sign-in failed',
    endpoint:id=>authPath(auth.profileId)+'?hostId='+encodeURIComponent(auth.hostId),eventsEndpoint:id=>authPath(auth.profileId)+'/events?hostId='+encodeURIComponent(auth.hostId)});
  $('provider-auth-close').onclick=()=>{$('provider-auth-terminal').hidden=true;};
  function kind(){return $('model-provider-kind').value;}
  function kindChanged(){
    const subscription=kind()!=='openai-compatible';
    $('model-provider-api-fields').hidden=subscription;$('model-provider-auth-note').hidden=!subscription;
    $('test-model-provider').hidden=subscription;$('load-model-provider-models').hidden=subscription;
  }
  $('model-provider-kind').onchange=()=>{kindChanged();$('model-provider-model').value=kind()==='claude-code'?'default':kind()==='chatgpt_codex'?'gpt-6.1-sol':'';};
  const endpoint=host=>'/api/model-providers?hostId='+encodeURIComponent(host||'local');
  const hostOptions=()=>[{id:'local',name:'Local',status:'connected'},...getHosts().filter(h=>h.kind==='remote')];
  const hostName=id=>hostOptions().find(h=>h.id===id)?.name||id;
  const checkedHosts=()=>[...$('model-provider-hosts').querySelectorAll('input:checked')].map(input=>input.value);
  function renderHosts(ids=['local']){
    const root=$('model-provider-hosts');root.replaceChildren();
    for(const h of hostOptions()){
      const label=el('label','model-provider-host-choice'),input=el('input');input.type='checkbox';input.value=h.id;input.checked=ids.includes(h.id);
      input.onchange=updateTestHost;label.append(input,document.createTextNode(h.name+(h.status==='connected'?'':' · offline')));root.append(label);
    }
    updateTestHost();
  }
  function testHost(){return hostOptions().find(h=>checkedHosts().includes(h.id)&&h.status==='connected')?.id;}
  function updateTestHost(){
    const id=testHost();$('model-provider-test-host').textContent=id?`Connection checks run from ${hostName(id)}.`:'Select a connected host to test the connection. Offline hosts can still be enabled.';
  }
  const message=text=>$('model-provider-message').textContent=text;
  const selection=()=>$('provider').value==='goose'?$('create-model-provider').value||null:null;
  const profile=()=>profiles.find(p=>p.id===selection());
  function draft(){return {
    ...(editing?{id:editing.id}:{}),name:$('model-provider-name').value.trim(),kind:kind(),baseUrl:kind()==='openai-compatible'?$('model-provider-url').value.trim():'',
    defaultModel:$('model-provider-model').value.trim(),hostIds:checkedHosts(),apiKey:kind()==='openai-compatible'?$('model-provider-key').value||null:null,removeKey:kind()!=='openai-compatible'||$('model-provider-remove-key').checked,
  };}
  function editor(p=null){
    editing=p;$('model-provider-editor').hidden=false;$('model-provider-kind').value=p?.kind||'openai-compatible';kindChanged();
    $('model-provider-editor-title').textContent=p?'Edit provider':'Add provider';
    $('model-provider-name').value=p?.name||'';$('model-provider-url').value=p?.baseUrl||'';
    $('model-provider-model').value=p?.defaultModel||'';$('model-provider-models').replaceChildren();
    $('model-provider-key').value='';$('model-provider-remove-key').checked=false;
    $('model-provider-key').placeholder=p?.keyConfigured?'Configured · leave blank to keep':'Optional';
    $('model-provider-key-note').textContent=p?.keyConfigured?'A key is saved for this provider and supplied only to enabled hosts. Its value is never returned.':'No API key is stored for this provider.';
    renderHosts(p?.hostIds||['local']);message('');$('model-provider-name').focus();
  }
  async function list(){
    const generation=++settingsGeneration;
    $('model-provider-list').replaceChildren(el('p','field-note','Loading providers…'));
    try{
      const result=await api('/api/model-providers');if(generation!==settingsGeneration)return;
      const list=$('model-provider-list');list.replaceChildren();
      if(!result.profiles.length)list.append(el('p','field-note','No providers configured. Add a provider, then choose which hosts can use it.'));
      for(const p of result.profiles){
        const card=el('div','model-provider-card'),copy=el('div');
        copy.append(el('strong','',p.name),el('p','field-note',p.kind==='chatgpt_codex'?'ChatGPT subscription · Codex OAuth':p.kind==='claude-code'?'Claude subscription · Claude Code':p.baseUrl),el('p','field-note',`${p.defaultModel} · ${p.kind==='chatgpt_codex'?(p.signedIn?'Signed in · shared with enabled hosts':'Not signed in'):p.kind==='claude-code'?'Sign-in managed on each host':p.keyConfigured?'API key configured':'No API key'}`),el('p','field-note','Available on: '+(p.hostIds?.map(hostName).join(', ')||'No hosts')));
        const actions=el('div','model-provider-actions'),edit=el('button','text-button','Edit'),remove=el('button','text-button','Delete');edit.type=remove.type='button';
        edit.onclick=()=>editor(p);
        remove.onclick=async()=>{
          if(!confirm(`Delete provider “${p.name}” and its stored key?`))return;
          try{await api('/api/model-providers',{method:'POST',body:JSON.stringify({operation:'delete',id:p.id})});if(editing?.id===p.id)closeEditor();await list();await loadCreate();}
          catch(e){message(e.message);}
        };
        if(p.kind==='chatgpt_codex'){
          const signin=el('button','text-button',p.signedIn?'Sign in again':'Sign in with ChatGPT');signin.type='button';signin.onclick=()=>signInCodex(p);actions.append(signin);
          const device=el('button','text-button','Device code');device.type='button';device.title='Experimental device-code sign-in';device.onclick=()=>signInCodex(p,true);actions.append(device);
          const rigDevice=el('button','text-button','Device code · Rig');rigDevice.type='button';rigDevice.title='Compare Rig’s experimental device-code sign-in';rigDevice.onclick=()=>signInCodex(p,'rig');actions.append(rigDevice);
          if(p.signedIn){const disconnect=el('button','text-button','Disconnect');disconnect.type='button';disconnect.onclick=async()=>{try{stopCodexPoll();await api(codexPath(p.id),{method:'DELETE'});$('codex-oauth').hidden=true;await list();}catch(error){message(error.message);}};actions.append(disconnect);}
        }
        if(p.kind==='claude-code')for(const hostId of p.hostIds||[]){
          const host=hostOptions().find(h=>h.id===hostId),signin=el('button','text-button','Sign in · '+hostName(hostId));signin.type='button';signin.disabled=host?.status!=='connected';
          signin.onclick=()=>{auth={id:p.id+':'+hostId,profileId:p.id,hostId};$('provider-auth-title').textContent=p.name+' · '+hostName(hostId);$('provider-auth-terminal').hidden=false;$('provider-auth-surface').scrollIntoView({block:'center'});authTerminal.open();};actions.append(signin);
        }
        actions.append(edit,remove);card.append(copy,actions);list.append(card);
      }
    }catch(e){if(generation===settingsGeneration){$('model-provider-list').replaceChildren();message(e.message);}}
  }
  function closeEditor(){editing=null;$('model-provider-editor').hidden=true;$('model-provider-key').value='';}
  async function action(operation){
    if(busy)return;busy=true;
    const selectedHost=operation==='save'?null:testHost(),generation=settingsGeneration,p=draft();
    for(const button of $('model-provider-editor').querySelectorAll('button'))button.disabled=true;
    message(operation==='save'?'Saving provider…':'Connecting to provider…');
    try{
      if(operation==='save'&&(!p.name||(p.kind==='openai-compatible'&&!p.baseUrl)||!p.defaultModel))throw Error('Enter a name, base URL, and default model. Load models to discover available IDs.');
      if(operation!=='save'&&!selectedHost)throw Error('Select a connected host to test this provider.');
      const result=await api(operation==='save'?'/api/model-providers':endpoint(selectedHost),{method:'POST',body:JSON.stringify({operation,profile:p})});
      if(generation!==settingsGeneration)return;
      if(operation==='save'){
        closeEditor();await list();await loadCreate();message('Provider saved. Available on '+(p.hostIds.map(hostName).join(', ')||'no hosts')+'.');
      }else{
        $('model-provider-models').replaceChildren(...result.options.map(o=>new Option(o.name||o.value,o.value)));
        message(`Connected from ${hostName(selectedHost)}. Model listing accepted${p.apiKey||editing?.keyConfigured?' the API key':''}; ${result.options.length} models found. No inference request was sent.`);
      }
    }catch(e){if(generation===settingsGeneration)message(e.message);}
    finally{busy=false;for(const button of $('model-provider-editor').querySelectorAll('button'))button.disabled=false;}
  }
  async function loadSettings(){closeEditor();message('');await list();}
  async function loadCreate(preferred){
    const generation=++createGeneration,select=$('create-model-provider');
    const previous=preferred===undefined?select.value:preferred;
    $('create-model-provider-field').hidden=$('provider').value!=='goose';
    if($('provider').value!=='goose'){profiles=[];return;}
    const selectedHost=$('create-host').value||'local';select.disabled=true;
    select.replaceChildren(new Option('Loading providers…',''));profiles=[];selectionChanged();
    try{
      const result=await api(endpoint(selectedHost));if(generation!==createGeneration)return;
      profiles=result.profiles;select.replaceChildren(new Option('Host environment configuration',''));
      for(const p of profiles)select.append(new Option(p.name,p.id));
      select.value=profiles.some(p=>p.id===previous)?previous:preferred!==undefined?'':profiles[0]?.id||'';
      $('create-model-provider-note').textContent=profiles.length?'Connection profiles are managed in Settings → Model providers.':'Add a provider in Settings, or use Goose configuration from this host’s environment.';
    }catch(e){if(generation!==createGeneration)return;select.replaceChildren(new Option('Host environment configuration',''));$('create-model-provider-note').textContent=e.message;}
    finally{if(generation===createGeneration){select.disabled=false;selectionChanged();}}
  }
  $('model-provider-editor').addEventListener('keydown',event=>{if(event.key==='Enter'&&event.target.tagName==='INPUT'){event.preventDefault();action('save');}});
  $('add-model-provider').onclick=()=>editor();$('cancel-model-provider').onclick=closeEditor;
  $('save-model-provider').onclick=()=>action('save');$('test-model-provider').onclick=()=>action('test');$('load-model-provider-models').onclick=()=>action('models');
  $('create-model-provider').onchange=selectionChanged;
  $('settings-dialog').addEventListener('close',()=>{$('model-provider-key').value='';settingsGeneration++;});
  return {loadSettings,loadCreate,selection,defaultModel:()=>profile()?.defaultModel};
}
