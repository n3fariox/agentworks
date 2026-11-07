// Session secrets in Settings: write-only values, encrypted on the server host.
// Values are never returned; the list shows only length and freshness.
export function setupSecrets({$,api,el,toast}){
  const root=$('secret-controls');
  root.innerHTML=`<legend>Secrets</legend>
    <p class="field-note">Named values for sessions to use. Values are encrypted on this server and are never shown again after saving. Changes save immediately.</p>
    <p class="field-note" data-key-note></p>
    <div class="project-field-heading"><h3>Stored secrets</h3><button type="button" class="text-button" data-new>＋ Add secret</button></div>
    <div data-list></div>
    <div data-editor hidden>
      <h3 data-title>Add secret</h3>
      <label for="secret-name">Name</label><input id="secret-name" maxlength="128" autocomplete="off" spellcheck="false" autocapitalize="characters" placeholder="GITHUB_TOKEN">
      <p class="field-note" data-name-note>Environment variable name: A–Z, 0–9 and _.</p>
      <label for="secret-description">Description <span class="optional">optional</span></label><input id="secret-description" maxlength="200" autocomplete="off">
      <label for="secret-value">Value</label><input id="secret-value" type="password" autocomplete="new-password" spellcheck="false">
      <p class="field-note" data-value-note></p>
      <div class="model-provider-actions"><button type="button" class="text-button" data-cancel>Cancel</button><button type="button" class="primary" data-save>Save secret</button></div>
    </div>
    <p class="error-text" role="alert" data-error></p>`;
  const q=s=>root.querySelector(s);
  const list=q('[data-list]'),editorRoot=q('[data-editor]'),error=q('[data-error]');
  const name=$('secret-name'),description=$('secret-description'),value=$('secret-value');
  let editing=null,generation=0,available=true;
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
      available=result.available;
      q('[data-key-note]').textContent=result.available
        ?`Encryption key file: ${result.keyFile}. It is not included in recovery backups; without it, stored values must be entered again. No password protects it yet, so programs running as your user, including agents, can read it.`
        :`Secret storage is unavailable: ${result.error}. Existing secrets cannot be read or changed until the key file is fixed.`;
      q('[data-new]').disabled=!available;
      list.replaceChildren(...(result.secrets.length?result.secrets.map(secret=>{
        const card=el('div','model-provider-card'),copy=el('div');
        const status=secret.readable===false?'Needs re-entry · stored with a different key file':`Set · ${plural(secret.length,'byte')} · updated ${new Date(secret.updated).toLocaleString()}`;
        copy.append(el('strong','secret-name',secret.name));
        if(secret.description)copy.append(el('p','field-note',secret.description));
        copy.append(el('p','field-note',status+(secret.sessions?` · used by ${plural(secret.sessions,'session')}`:'')));
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
  return {refresh,close:closeEditor};
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
      note.textContent='Environment variables are set when the harness starts. Changing them restarts an idle session, or a busy one after its turn. Tool-only delivery is not available yet.';
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
    if(kind==='secrets_unavailable')return 'Not available to this harness: '+d.secrets.join(', ')+'. Enter the values again in Settings → Secrets, then restart the session.';
    if(kind!=='secret_assignments')return null;
    return d.secrets.length?'Session secrets: '+d.secrets.join(', '):'Session secrets removed';
  }
  return {load,save,notice};
}
