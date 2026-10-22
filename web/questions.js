// L-02 questions/elicitation. The server owns pending questions and validates answers;
// this module renders ACP form elicitations, mirrors validation for immediate feedback,
// and keeps drafts plus a stable submission ID in storage so redraws, reconnects and
// reloads neither lose an answer nor send it twice.
const RADIO_LIMIT=8;
const INPUT_TYPES={email:'email',uri:'url',date:'date','date-time':'datetime-local'};

function options(schema){
  if(Array.isArray(schema?.enum))return schema.enum.filter(v=>typeof v==='string').map(v=>({value:v,title:v}));
  return (schema?.oneOf||schema?.anyOf||[]).filter(o=>typeof o?.const==='string').map(o=>({value:o.const,title:o.title||o.const,description:o.description||''}));
}

// Normalized field list for a form request, in the agent's property order.
export function formFields(request){
  const schema=request?.requestedSchema||{};const required=new Set(schema.required||[]);
  return Object.entries(schema.properties||{}).map(([name,p])=>{
    const base={name,title:p.title||name,description:p.description||'',required:required.has(name),default:p.default};
    if(p.type==='array')return {...base,kind:'multi',options:options(p.items),minItems:p.minItems,maxItems:p.maxItems};
    if(p.type==='string'&&options(p).length)return {...base,kind:'single',options:options(p)};
    if(p.type==='string')return {...base,kind:'text',format:p.format,minLength:p.minLength,maxLength:p.maxLength,pattern:p.pattern};
    if(p.type==='number'||p.type==='integer')return {...base,kind:p.type,minimum:p.minimum,maximum:p.maximum};
    if(p.type==='boolean')return {...base,kind:'boolean'};
    return {...base,kind:'unsupported'};
  });
}

export function initialValues(fields){
  return Object.fromEntries(fields.map(f=>[f.name,
    f.kind==='multi'?(Array.isArray(f.default)?f.default:[]):f.kind==='boolean'?f.default===true:f.default==null?'':String(f.default)]));
}

const plural=(n,word)=>`${n} ${word}${n===1?'':'s'}`;
function validFormat(format,value){
  if(format==='email')return /^[^\s@]+@[^\s@.][^\s@]*\.[^\s@]*[^\s@.]$/.test(value);
  if(format==='uri')return /^[a-z][a-z0-9+.-]*:\S+$/i.test(value);
  if(format==='date')return /^\d{4}-\d{2}-\d{2}$/.test(value)&&!Number.isNaN(Date.parse(value+'T00:00:00Z'))&&new Date(value+'T00:00:00Z').toISOString().startsWith(value);
  return true;
}

// Raw form values -> ACP content, with per-field errors that mirror the server's checks.
// Empty optional fields are omitted rather than sent as empty strings.
export function collect(fields,values){
  const content={},errors={};
  for(const f of fields){
    const raw=values[f.name];
    const fail=message=>{errors[f.name]=message;};
    if(f.kind==='multi'){
      const picked=(Array.isArray(raw)?raw:[]).filter(v=>f.options.some(o=>o.value===v));
      if(f.required&&!picked.length){fail(`${f.title} is required`);continue;}
      if(f.minItems!=null&&picked.length<f.minItems){fail(`Choose at least ${plural(f.minItems,'option')}`);continue;}
      if(f.maxItems!=null&&picked.length>f.maxItems){fail(`Choose at most ${plural(f.maxItems,'option')}`);continue;}
      if(picked.length||f.required)content[f.name]=picked;
      continue;
    }
    if(f.kind==='boolean'){content[f.name]=!!raw;continue;}
    const text=typeof raw==='string'?(f.kind==='text'?raw:raw.trim()):'';
    if(!text){if(f.required)fail(`${f.title} is required`);continue;}
    if(f.kind==='single'){
      if(!f.options.some(o=>o.value===text))fail(`${f.title} is not one of the offered choices`);else content[f.name]=text;
    }else if(f.kind==='number'||f.kind==='integer'){
      const n=Number(text);
      if(!Number.isFinite(n))fail(`${f.title} must be a number`);
      else if(f.kind==='integer'&&!Number.isInteger(n))fail(`${f.title} must be a whole number`);
      else if(f.minimum!=null&&n<f.minimum)fail(`${f.title} must be at least ${f.minimum}`);
      else if(f.maximum!=null&&n>f.maximum)fail(`${f.title} must be at most ${f.maximum}`);
      else content[f.name]=n;
    }else if(f.kind==='text'){
      let value=text;
      if(f.format==='date-time'){
        const at=new Date(text);
        if(Number.isNaN(at.getTime())){fail(`${f.title} must be a valid date and time`);continue;}
        value=at.toISOString();
      }
      const length=[...value].length;
      if(f.minLength!=null&&length<f.minLength)fail(`${f.title} must be at least ${plural(f.minLength,'character')}`);
      else if(f.maxLength!=null&&length>f.maxLength)fail(`${f.title} must be at most ${plural(f.maxLength,'character')}`);
      else if(f.format&&!validFormat(f.format,value))fail(`${f.title} must be a valid ${f.format}`);
      else if(f.pattern&&!(()=>{try{return new RegExp(f.pattern,'u').test(value);}catch{return true;}})())fail(`${f.title} is not in the expected format`);
      else content[f.name]=value;
    }else fail(`${f.title} uses an unsupported input type`);
  }
  return {content,errors};
}

// Retries of an unchanged answer reuse the submission ID so the server can dedupe them.
export function submissionFor(draft,payload,uuid){
  const key=JSON.stringify(payload);
  if(draft.sent?.key===key)return draft.sent.id;
  draft.sent={key,id:uuid()};return draft.sent.id;
}

export function setupQuestions({api,uuid,storage,getActive,isConnected,agentName}){
  const cards=new Map();
  const prefix=sid=>`agentworks-question:${sid}:`;
  const load=(sid,id)=>{try{return JSON.parse(storage.getItem(prefix(sid)+id))||{};}catch{return {};}};
  const store=(sid,id,draft)=>{try{storage.setItem(prefix(sid)+id,JSON.stringify(draft));}catch{}};
  const forget=(sid,id)=>{try{storage.removeItem(prefix(sid)+id);}catch{}};

  function el(tag,cls,text){const n=document.createElement(tag);if(cls)n.className=cls;if(text!=null)n.textContent=text;return n;}

  function build(sid,id,entry){
    const request=entry.request||{};const fields=entry.unsupported?[]:formFields(request);
    const draft=load(sid,id);const values={...initialValues(fields),...(draft.values||{})};
    const form=el('form','approval question');form.noValidate=true;form.dataset.questionId=id;
    const heading=el('h4','',`Question from ${agentName(request)}`);heading.id=`q-${id}-title`;form.setAttribute('aria-labelledby',heading.id);
    form.append(heading,el('p','question-message',request.message||'The agent needs your input.'));
    if(entry.unsupported){const note=el('p','question-unsupported',entry.unsupported);note.setAttribute('role','note');form.append(note);}
    const summary=el('p','question-error');summary.setAttribute('role','alert');summary.hidden=true;form.append(summary);
    const status=el('p','question-status');status.setAttribute('role','status');form.append(status);
    const inputs=new Map();
    const save=()=>{draft.values=read();store(sid,id,draft);};
    const read=()=>Object.fromEntries(fields.map(f=>{
      const nodes=inputs.get(f.name);
      if(f.kind==='multi')return [f.name,nodes.filter(n=>n.checked).map(n=>n.value)];
      if(f.kind==='boolean')return [f.name,nodes[0].checked];
      if(f.kind==='single'&&nodes[0].type==='radio')return [f.name,nodes.find(n=>n.checked)?.value||''];
      return [f.name,nodes[0].value];
    }));
    for(const f of fields){
      const wrap=el(f.kind==='single'&&f.options.length<=RADIO_LIMIT||f.kind==='multi'?'fieldset':'div','question-field');wrap.dataset.field=f.name;
      const errorNode=el('p','field-error');errorNode.id=`q-${id}-${f.name}-error`;errorNode.hidden=true;
      const describe=[];
      const caption=(node)=>{node.append(document.createTextNode(f.title));if(f.required){const mark=el('span','required',' (required)');node.append(mark);}};
      const help=f.description?el('p','field-help',f.description):null;if(help){help.id=`q-${id}-${f.name}-help`;describe.push(help.id);}
      describe.push(errorNode.id);
      let nodes=[];
      if(wrap.tagName==='FIELDSET'){
        const legend=el('legend');caption(legend);wrap.append(legend);if(help)wrap.append(help);
        wrap.setAttribute('aria-describedby',describe.join(' '));
        if(f.kind==='multi'&&f.maxItems!=null)wrap.append(el('p','field-help',`Choose up to ${plural(f.maxItems,'option')}.`));
        for(const option of f.options){
          const label=el('label','question-option');const input=el('input');
          input.type=f.kind==='multi'?'checkbox':'radio';input.name=`q-${id}-${f.name}`;input.value=option.value;
          input.checked=f.kind==='multi'?values[f.name].includes(option.value):values[f.name]===option.value;
          const text=el('span','option-text');text.append(el('span','option-title',option.title));if(option.description)text.append(el('span','option-description',option.description));
          label.append(input,text);wrap.append(label);nodes.push(input);
        }
        if(f.kind==='single'&&!f.required){
          const clear=el('button','question-clear','Clear choice');clear.type='button';
          clear.onclick=()=>{for(const n of nodes)n.checked=false;save();nodes[0]?.focus();};wrap.append(clear);
        }
      }else{
        const label=el('label','field-label');const input=f.kind==='single'?el('select'):el('input');
        input.id=`q-${id}-${f.name}`;label.htmlFor=input.id;caption(label);
        if(f.kind==='single'){
          input.append(el('option','',f.required?'Choose…':'No answer'));input.firstChild.value='';
          for(const option of f.options){const o=el('option','',option.title);o.value=option.value;input.append(o);}
          input.value=values[f.name];
        }else if(f.kind==='boolean'){
          input.type='checkbox';input.checked=!!values[f.name];label.classList.add('question-option');label.replaceChildren(input,el('span','option-title',f.title));
        }else if(f.kind==='number'||f.kind==='integer'){
          input.type='number';input.inputMode=f.kind==='integer'?'numeric':'decimal';input.step=f.kind==='integer'?'1':'any';
          if(f.minimum!=null)input.min=f.minimum;if(f.maximum!=null)input.max=f.maximum;input.value=values[f.name];
        }else{
          input.type=INPUT_TYPES[f.format]||'text';if(f.maxLength!=null)input.maxLength=f.maxLength;input.value=values[f.name];
          if(f.format==='email')input.autocomplete='email';
        }
        if(f.required)input.setAttribute('aria-required','true');
        input.setAttribute('aria-describedby',describe.join(' '));
        wrap.append(label);if(f.kind!=='boolean')wrap.append(input);if(help)wrap.append(help);
        nodes=[input];
      }
      wrap.append(errorNode);inputs.set(f.name,nodes);
      wrap.addEventListener('input',()=>{save();errorNode.hidden=true;for(const n of nodes)n.removeAttribute('aria-invalid');});
      wrap.addEventListener('change',save);
      form.append(wrap);
    }
    const actions=el('div','question-actions');
    const submit=el('button','primary','Submit answer');submit.type='submit';
    const skip=el('button','','Skip');skip.type='button';skip.title='Tell the agent you chose not to answer';
    const cancel=el('button','reject','Cancel');cancel.type='button';cancel.title='Stop the agent action that asked this question';
    if(entry.unsupported)submit.hidden=true;
    actions.append(submit,skip,cancel);form.append(actions);
    const card={form,sending:false,draft,buttons:[submit,skip,cancel],refresh(){for(const b of this.buttons)b.disabled=this.sending||!isConnected();}};

    function showErrors(errors,message){
      let first=null;
      for(const f of fields){
        const node=form.querySelector(`[data-field="${CSS.escape(f.name)}"] .field-error`);const nodes=inputs.get(f.name);
        if(errors[f.name]){node.textContent=errors[f.name];node.hidden=false;for(const n of nodes)n.setAttribute('aria-invalid','true');first??=nodes[0];}
        else{node.hidden=true;for(const n of nodes)n.removeAttribute('aria-invalid');}
      }
      summary.textContent=message||'';summary.hidden=!message;
      first?.focus();
    }
    async function send(action,content){
      if(card.sending)return;
      const payload={action,...(action==='accept'?{content}:{})};
      const submissionId=submissionFor(draft,payload,uuid);store(sid,id,draft);
      card.sending=true;card.refresh();status.textContent='Sending…';
      try{
        await api(`/api/sessions/${encodeURIComponent(sid)}/questions/${encodeURIComponent(id)}`,{method:'POST',body:JSON.stringify({...payload,submissionId})});
        status.textContent=action==='accept'?'Answer sent.':action==='decline'?'Skipped.':'Cancelled.';
        // Keep the buttons disabled until the server's resolution event removes the card.
        return;
      }catch(error){
        status.textContent='';
        showErrors({},error.status?error.message:'Not delivered: the connection to Agentworks was lost. Your answer is kept here; submit again once reconnected.');
      }
      card.sending=false;card.refresh();
    }
    form.addEventListener('submit',event=>{
      event.preventDefault();
      const {content,errors}=collect(fields,read());
      if(Object.keys(errors).length){showErrors(errors,`Please fix ${plural(Object.keys(errors).length,'field')} before submitting.`);return;}
      showErrors({});send('accept',content);
    });
    skip.onclick=()=>send('decline');
    cancel.onclick=()=>send('cancel');
    card.refresh();
    return card;
  }

  // Patch the area in place: existing cards keep focus, caret and typed values.
  function sync(area,entries){
    const sid=getActive()?.id;if(!sid)return;
    const live=new Set(entries.map(([id])=>id));
    for(const [id,card] of cards)if(!live.has(id)||!card.form.isConnected){card.form.remove();cards.delete(id);}
    for(const [id,entry] of entries){
      let card=cards.get(id);
      if(!card){card=build(sid,id,entry);cards.set(id,card);area.prepend(card.form);}
      card.refresh();
    }
  }
  // Connection changes do not redraw; re-check button availability directly.
  function refresh(){for(const card of cards.values())card.refresh();}
  // Resolution events (live or replayed) end a question for good; drop its draft.
  return {sync,refresh,forget:id=>{const sid=getActive()?.id;if(sid)forget(sid,id);}};
}
