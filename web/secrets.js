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
        copy.append(el('p','field-note',status));
        const actions=el('div','model-provider-actions'),edit=el('button','text-button','Edit'),remove=el('button','text-button','Delete');
        edit.type=remove.type='button';edit.disabled=!available;
        edit.onclick=()=>editor(secret);
        remove.onclick=async()=>{
          if(!confirm(`Delete secret ${secret.name}? Its value cannot be recovered.`))return;
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
