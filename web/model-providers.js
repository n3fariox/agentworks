// Shared provider definitions, host availability, and session selection.
export function setupModelProviders({$,api,el,getHosts,selectionChanged}) {
  let editing=null,profiles=[],settingsGeneration=0,createGeneration=0,busy=false;
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
    ...(editing?{id:editing.id}:{}),name:$('model-provider-name').value.trim(),baseUrl:$('model-provider-url').value.trim(),
    defaultModel:$('model-provider-model').value.trim(),hostIds:checkedHosts(),apiKey:$('model-provider-key').value||null,removeKey:$('model-provider-remove-key').checked,
  };}
  function editor(p=null){
    editing=p;$('model-provider-editor').hidden=false;
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
        copy.append(el('strong','',p.name),el('p','field-note',p.baseUrl),el('p','field-note',`${p.defaultModel} · ${p.keyConfigured?'API key configured':'No API key'}`),el('p','field-note','Available on: '+(p.hostIds?.map(hostName).join(', ')||'No hosts')));
        const actions=el('div','model-provider-actions'),edit=el('button','text-button','Edit'),remove=el('button','text-button','Delete');edit.type=remove.type='button';
        edit.onclick=()=>editor(p);
        remove.onclick=async()=>{
          if(!confirm(`Delete provider “${p.name}” and its stored key?`))return;
          try{await api('/api/model-providers',{method:'POST',body:JSON.stringify({operation:'delete',id:p.id})});if(editing?.id===p.id)closeEditor();await list();await loadCreate();}
          catch(e){message(e.message);}
        };
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
      if(operation==='save'&&(!p.name||!p.baseUrl||!p.defaultModel))throw Error('Enter a name, base URL, and default model. Load models to discover available IDs.');
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
