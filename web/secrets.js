// Session secrets in Settings: write-only values, encrypted on the server host.
// Values are never returned; the list shows only length and freshness.
const LOCK_TIMES=[[15,'15 minutes'],[60,'1 hour'],[240,'4 hours'],[720,'12 hours'],[1440,'24 hours'],[0,'Never']];

// The Unlock secrets banner and dialog. `refresh` reads the lock state; `onChange`
// runs after an unlock so other views can reload.
export function setupSecretsLock({$,api,toast,onChange}){
  const banner=$('secrets-banner'),dialog=$('secrets-unlock-dialog'),password=$('secrets-unlock-password'),error=$('secrets-unlock-error');
  let state=null,generation=0;
  function show(status){state=status;banner.hidden=!status?.locked;}
  async function refresh(){
    const current=++generation;
    try{const status=await api('/api/secrets/status');if(current===generation)show(status);return status;}catch{return state;}
  }
  function open(){error.textContent='';password.value='';dialog.showModal();password.focus();}
  $('secrets-banner-unlock').onclick=open;
  $('secrets-unlock-dismiss').onclick=()=>dialog.close();
  dialog.addEventListener('close',()=>{password.value='';});
  $('secrets-unlock-form').onsubmit=async event=>{
    event.preventDefault();error.textContent='';
    const submit=dialog.querySelector('button[type=submit]');submit.disabled=true;
    try{
      const status=await api('/api/secrets/unlock',{method:'POST',body:JSON.stringify({password:password.value})});
      show(status);dialog.close();toast?.('Secrets unlocked','success');onChange?.(status);
    }catch(e){error.textContent=e.message;password.select();}
    finally{submit.disabled=false;}
  };
  document.addEventListener('visibilitychange',()=>{if(!document.hidden)refresh();});
  refresh();
  return {refresh,open,show,get state(){return state;}};
}

export function setupSecrets({$,api,el,toast,lock}){
  const root=$('secret-controls');
  root.innerHTML=`<legend>Secrets</legend>
    <p class="field-note">Named values for sessions to use. Values are encrypted on this server and are never shown again after saving. Changes save immediately.</p>
    <p class="field-note" data-key-note></p>
    <div data-password-section>
      <h3>Password</h3>
      <p class="field-note" data-password-note></p>
      <div class="secret-password-actions">
        <button type="button" class="settings-action" data-unlock hidden>Unlock secrets</button>
        <button type="button" class="settings-action" data-password-edit></button>
        <button type="button" class="settings-action" data-password-remove hidden>Remove password</button>
        <button type="button" class="settings-action" data-lock hidden>Lock now</button>
      </div>
      <div class="secret-password-form" data-password-form hidden>
        <input type="text" autocomplete="username" value="agentworks-secrets" hidden aria-hidden="true" tabindex="-1">
        <label for="secret-password-current" data-current-label>Current password</label><input id="secret-password-current" type="password" autocomplete="current-password">
        <label for="secret-password-new" data-new-label>New password</label><input id="secret-password-new" type="password" autocomplete="new-password">
        <label for="secret-password-confirm" data-confirm-label>Confirm new password</label><input id="secret-password-confirm" type="password" autocomplete="new-password">
        <p class="field-note" data-password-help>At least 8 characters. It cannot be recovered: if you forget it, stored values must be entered again with a new key file.</p>
        <div class="model-provider-actions"><button type="button" class="text-button" data-password-cancel>Cancel</button><button type="button" class="primary" data-password-save>Save password</button></div>
      </div>
      <label for="secret-lock-minutes">Lock after inactivity</label><select id="secret-lock-minutes"></select>
      <p class="field-note">Counted from the last time a session used a secret. Locking drops the key from memory.</p>
    </div>
    <div class="project-field-heading"><h3>Stored secrets</h3><button type="button" class="text-button" data-new>＋ Add secret</button></div>
    <div data-list></div>
    <div data-editor hidden>
      <h3 data-title>Add secret</h3>
      <label for="secret-name">Name</label><input id="secret-name" maxlength="128" autocomplete="off" spellcheck="false" autocapitalize="characters" placeholder="GITHUB_TOKEN">
      <p class="field-note" data-name-note>Environment variable name: A–Z, 0–9 and _.</p>
      <label for="secret-description">Description for agents <span class="optional">optional</span></label><input id="secret-description" maxlength="200" autocomplete="off" placeholder="Read-only token for the acme GitHub org">
      <p class="field-note">What it's for and what it can access. Agents assigned this secret can read this text.</p>
      <label for="secret-value">Value</label><input id="secret-value" type="password" autocomplete="new-password" spellcheck="false">
      <p class="field-note" data-value-note></p>
      <div class="model-provider-actions"><button type="button" class="text-button" data-cancel>Cancel</button><button type="button" class="primary" data-save>Save secret</button></div>
    </div>
    <p class="error-text" role="alert" data-error></p>`;
  const q=s=>root.querySelector(s);
  const list=q('[data-list]'),editorRoot=q('[data-editor]'),error=q('[data-error]');
  const name=$('secret-name'),description=$('secret-description'),value=$('secret-value');
  let editing=null,generation=0,available=true,status=null,passwordMode=null;
  const current=$('secret-password-current'),fresh=$('secret-password-new'),confirmPassword=$('secret-password-confirm'),lockMinutes=$('secret-lock-minutes');
  for(const [minutes,label] of LOCK_TIMES){const o=el('option','',label);o.value=String(minutes);lockMinutes.append(o);}
  function closePassword(){q('[data-password-form]').hidden=true;passwordMode=null;current.value=fresh.value=confirmPassword.value='';}
  function passwordForm(mode){
    passwordMode=mode;error.textContent='';q('[data-password-form]').hidden=false;
    const removing=mode==='remove';
    current.hidden=q('[data-current-label]').hidden=!status.password;
    for(const node of [fresh,confirmPassword,q('[data-new-label]'),q('[data-confirm-label]'),q('[data-password-help]')])node.hidden=removing;
    q('[data-password-save]').textContent=removing?'Remove password':'Save password';
    (status.password?current:fresh).focus();
  }
  function renderPassword(){
    const section=q('[data-password-section]');section.hidden=!status.available&&!status.locked;
    q('[data-password-note]').textContent=!status.password
      ?'No password. Programs running as your user, including agents, can read the key file and decrypt stored values. A password keeps the key encrypted until you unlock it.'
      :status.locked?'Locked. Sessions start without their environment secrets, and commands that need a secret wait until you unlock.'
      :'Unlocked. The password is never stored; the key is held in memory until it locks.';
    q('[data-unlock]').hidden=!status.locked;
    q('[data-password-edit]').hidden=status.locked;q('[data-password-edit]').textContent=status.password?'Change password':'Set password';
    q('[data-password-remove]').hidden=q('[data-lock]').hidden=status.locked||!status.password;
    lockMinutes.value=String(status.lockMinutes);lockMinutes.disabled=!status.password;
    if(status.locked)closePassword();
  }
  q('[data-unlock]').onclick=()=>lock?.open();
  q('[data-password-edit]').onclick=()=>passwordForm(status.password?'change':'set');
  q('[data-password-remove]').onclick=()=>passwordForm('remove');
  q('[data-password-cancel]').onclick=closePassword;
  q('[data-lock]').onclick=async()=>{
    try{lock?.show(await api('/api/secrets/lock',{method:'POST'}));await refresh();toast?.('Secrets locked','success');}catch(e){error.textContent=e.message;}
  };
  q('[data-password-save]').onclick=async()=>{
    error.textContent='';
    const removing=passwordMode==='remove';
    if(!removing&&fresh.value!==confirmPassword.value){error.textContent='The new passwords do not match.';return;}
    if(!removing&&!fresh.value){error.textContent='Enter a new password.';return;}
    const save=q('[data-password-save]');save.disabled=true;
    try{
      lock?.show(await api('/api/secrets/password',{method:'PUT',body:JSON.stringify({current:current.value,password:removing?'':fresh.value})}));
      closePassword();await refresh();toast?.(removing?'Password removed':'Password saved','success');
    }catch(e){error.textContent=e.message;}finally{save.disabled=false;}
  };
  lockMinutes.onchange=async()=>{
    try{await api('/api/secrets/lock-timeout',{method:'PUT',body:JSON.stringify({minutes:Number(lockMinutes.value)})});}
    catch(e){error.textContent=e.message;lockMinutes.value=String(status.lockMinutes);}
  };
  const plural=(n,word)=>`${n} ${word}${n===1?'':'s'}`;
  function closeEditor(){editorRoot.hidden=true;editing=null;value.value='';error.textContent='';}
  function editor(secret){
    editing=secret||null;error.textContent='';editorRoot.hidden=false;
    q('[data-title]').textContent=secret?`Edit ${secret.name}`:'Add secret';
    name.value=secret?.name||'';name.readOnly=Boolean(secret);q('[data-name-note]').hidden=Boolean(secret);
    description.value=secret?.description||'';value.value='';
    value.placeholder=secret?'Leave blank to keep the current value':'';
    q('[data-value-note]').textContent=secret?.readable===false?'The stored value cannot be read with the current key file. Enter it again.':secret?'':'Shown only while you type it here.';
    (secret?value:name).focus();
  }
  async function refresh(){
    const current=++generation;
    try{
      const result=await api('/api/secrets');if(current!==generation)return;
      status=result;lock?.show(result);
      available=result.available&&!result.locked;
      q('[data-key-note]').textContent=result.available
        ?`Encryption key file: ${result.keyFile}. It is not included in recovery backups; without it, stored values must be entered again.`
        :`Secret storage is unavailable: ${result.error}. Existing secrets cannot be read or changed until the key file is fixed.`;
      renderPassword();
      q('[data-new]').disabled=!available;
      list.replaceChildren(...(result.secrets.length?result.secrets.map(secret=>{
        const card=el('div','model-provider-card'),copy=el('div');
        const state=secret.readable===false?'Needs re-entry · stored with a different key file':`${result.locked?'Locked':'Set'} · ${plural(secret.length,'byte')} · updated ${new Date(secret.updated).toLocaleString()}`;
        copy.append(el('strong','secret-name',secret.name));
        if(secret.description)copy.append(el('p','field-note',secret.description));
        copy.append(el('p','field-note',state+(secret.sessions?` · used by ${plural(secret.sessions,'session')}`:'')));
        const actions=el('div','model-provider-actions'),edit=el('button','text-button','Edit'),remove=el('button','text-button','Delete');
        edit.type=remove.type='button';edit.disabled=!available;
        edit.onclick=()=>editor(secret);
        remove.onclick=async()=>{
          if(!confirm(`Delete secret ${secret.name}? Its value cannot be recovered${secret.sessions?` and it is removed from ${plural(secret.sessions,'session')}`:''}.`))return;
          try{await api(`/api/secrets/${encodeURIComponent(secret.id)}`,{method:'DELETE'});if(editing?.id===secret.id)closeEditor();await refresh();}
          catch(e){error.textContent=e.message;}
        };
        actions.append(edit,remove);card.append(copy,actions);return card;
      }):[el('p','field-note','No secrets yet.')]));
    }catch(e){if(current===generation)list.replaceChildren(el('p','error-text',e.message));}
  }
  q('[data-new]').onclick=()=>editor(null);
  q('[data-cancel]').onclick=closeEditor;
  name.oninput=()=>{const at=name.selectionStart;name.value=name.value.toUpperCase();name.setSelectionRange(at,at);};
  q('[data-save]').onclick=async()=>{
    error.textContent='';q('[data-save]').disabled=true;
    try{
      if(editing){
        const body={description:description.value};if(value.value)body.value=value.value;
        await api(`/api/secrets/${encodeURIComponent(editing.id)}`,{method:'PUT',body:JSON.stringify(body)});
      }else{
        await api('/api/secrets',{method:'POST',body:JSON.stringify({name:name.value.trim(),description:description.value,value:value.value})});
      }
      const saved=editing?.name||name.value.trim();closeEditor();await refresh();toast?.(`Saved ${saved}`,'success');
    }catch(e){error.textContent=e.message;}finally{q('[data-save]').disabled=false;}
  };
  return {refresh,close(){closeEditor();closePassword();}};
}

// Session settings: which stored secrets this session may use, and how.
const DELIVERY=[['tool','Tool only'],['environment','Environment variable']];
const DELEGATION=[['restricted','Restricted'],['passable','Passable to sessions it creates']];
export function setupSessionSecrets({$,api,el,getActive}){
  const root=$('session-secret-controls');
  root.innerHTML=`<legend>Secrets</legend>
    <p class="field-note">Only secrets listed here can be used by this session. Tool only keeps the value out of the agent's environment; an environment variable also reaches every command and subagent the harness runs.</p>
    <div class="session-secret-list" data-list></div>
    <div class="model-field"><select data-add aria-label="Secret to add"></select><button type="button" class="text-button" data-add-button>Add</button></div>
    <p class="field-note" role="status" data-note></p>`;
  const q=s=>root.querySelector(s);
  const list=q('[data-list]'),add=q('[data-add]'),note=q('[data-note]');
  let secrets=[],rows=[],session=null;
  const select=(options,value,label)=>{const node=el('select');node.setAttribute('aria-label',label);for(const [v,l] of options){const o=el('option','',l);o.value=v;node.append(o);}node.value=value;return node;};
  function render(){
    list.replaceChildren(...(rows.length?rows.map((row,index)=>{
      const secret=secrets.find(s=>s.id===row.secretId);
      const item=el('div','session-secret'),heading=el('div','session-secret-heading');
      heading.append(el('strong','secret-name',secret?.name||row.name));
      if(row.origin&&row.origin!=='human')heading.append(el('span','field-note',' · passed on by another session'));
      if(secret?.readable===false)heading.append(el('span','error-text',' · needs re-entry in Settings'));
      const remove=el('button','text-button','Remove');remove.type='button';remove.onclick=()=>{rows.splice(index,1);render();};
      heading.append(remove);
      const delivery=select(DELIVERY,row.delivery,`${secret?.name} delivery`);delivery.onchange=()=>{row.delivery=delivery.value;};
      const delegation=select(DELEGATION,row.delegation,`${secret?.name} delegation`);delegation.onchange=()=>{row.delegation=delegation.value;};
      const ask=el('label','workspace-check'),box=el('input');box.type='checkbox';box.checked=row.approval==='eachUse';box.onchange=()=>{row.approval=box.checked?'eachUse':'none';};
      ask.append(box,' Ask me before each tool use');
      item.append(heading,delivery,delegation,ask);return item;
    }):[el('p','field-note','No secrets assigned.')]));
    const unassigned=secrets.filter(s=>!rows.some(r=>r.secretId===s.id));
    add.replaceChildren(...(unassigned.length?unassigned.map(s=>{const o=el('option','',s.description?`${s.name} · ${s.description}`:s.name);o.value=s.id;return o;}):[el('option','','No other secrets · add them in Settings')]));
    add.disabled=q('[data-add-button]').disabled=!unassigned.length;
  }
  q('[data-add-button]').onclick=()=>{
    const secret=secrets.find(s=>s.id===add.value);if(!secret)return;
    rows.push({secretId:secret.id,name:secret.name,delivery:'tool',delegation:'restricted',approval:'none',origin:'human'});render();
  };
  async function load(){
    session=getActive();note.textContent='';rows=[];secrets=[];
    root.hidden=!session;if(!session)return;
    try{
      const result=await api(`/api/sessions/${encodeURIComponent(session.id)}/secrets`);
      secrets=result.secrets;rows=result.assignments.map(a=>({...a}));
      note.textContent='Environment variables are set when the harness starts. Changing them restarts an idle session, or a busy one after its turn. Tool-only changes apply to the next call.';
    }catch(e){note.textContent='Cannot load secrets: '+e.message;}
    render();
  }
  async function save(){
    if(!session)return;
    try{
      const body={assignments:rows.map(({secretId,delivery,delegation,approval})=>({secretId,delivery,delegation,approval}))};
      const result=await api(`/api/sessions/${encodeURIComponent(session.id)}/secrets`,{method:'PUT',body:JSON.stringify(body)});
      rows=result.assignments.map(a=>({...a}));render();
    }catch(e){note.textContent='Secrets not saved: '+e.message;throw e;}
  }
  function notice(kind,d){
    if(kind==='secrets_unavailable')return d.locked
      ?'Secrets were locked when this harness started: '+d.secrets.join(', ')+'. Unlocking them restarts the session with them.'
      :'Not available to this harness: '+d.secrets.join(', ')+'. Enter the values again in Settings → Secrets, then restart the session.';
    if(kind==='secrets_locked')return 'Waiting for you to unlock secrets to run a command with '+d.secrets.join(', ')+'.';
    if(kind!=='secret_assignments')return null;
    return d.secrets.length?'Session secrets: '+d.secrets.join(', '):'Session secrets removed';
  }
  return {load,save,notice};
}
