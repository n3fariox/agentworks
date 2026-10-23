// Starlark hook registrations in Settings: list, enable, edit and install.
// Changes save immediately. The server compiles and type-checks a script before it
// is stored, and reports its diagnostic here; nothing is loaded from disk.
// Starter for a new parser; scripts/progress-smoke.mjs checks that it installs.
export const TEMPLATE=String.raw`load("@agentworks//progress.star", "Counts", "LineContext", "Progress")

# Only output of tool commands matching this regex reaches on_line.
COMMAND_PATTERN = "^make\\b"
DISPLAY_NAME = "Make"

def on_line(ctx: LineContext, line: str) -> Progress | None:
    # ctx.state persists between lines of one output stream (plain data only).
    m = regex_match("^\\[(\\d+)/(\\d+)\\]", line)
    if m != None:
        ctx.state["done"] = int(m[0])
        ctx.state["total"] = int(m[1])
    if "total" not in ctx.state:
        return None
    counts = [Counts(done = ctx.state["done"], total = ctx.state["total"], unit = "steps")]
    return Progress(phase = "Building", label = None, counts = counts)

agent.register_progress(on_line, command_pattern = COMMAND_PATTERN, display_name = DISPLAY_NAME)
`;

export const FORMATTER_TEMPLATE=String.raw`load("@agentworks//message.star", "MessageContext", "PeerMessage")

def format_message(ctx: MessageContext, message: PeerMessage) -> str:
    return message.text

agent.register_formatter(format_message, message_type = "inbox.peer", text_pattern = "^Review")
`;
export const HEADER_TEMPLATE=String.raw`load("@agentworks//tool.star", "ToolCall", "ToolContext")

# Return the one-line tool card header, or None to keep the harness title.
def tool_header(ctx: ToolContext, tool: ToolCall) -> str | None:
    return tool.description

agent.register_tool_header(tool_header, title_pattern = ".*")
`;
const TEMPLATES={progress:TEMPLATE,formatter:FORMATTER_TEMPLATE,header:HEADER_TEMPLATE,both:TEMPLATE+'\n'+FORMATTER_TEMPLATE};

export function setupParserScripts({$,api,el}){
  const root=$('parser-script-controls');
  root.innerHTML=`<legend>Starlark scripts</legend>
    <p class="field-note">Scripts register tool-progress, tool-header and message-formatting hooks. Enabled handlers run in filename order, then registration order; the first matching handler for each hook wins. Each script can register several kinds. Changes apply to future output and inbox deliveries; tool headers are recomputed for open sessions.</p>
    <div class="parser-script-list" role="list" data-list></div>
    <button type="button" class="text-button parser-script-new" data-new>＋ New script</button>
    <div class="field-note" role="status" data-note></div>
    <div class="parser-script-editor" data-editor hidden>
      <label data-starter-label>Starter<select data-starter><option value="progress">Tool progress</option><option value="formatter">Inbox formatter</option><option value="header">Tool header</option><option value="both">Progress and inbox formatter</option></select></label>
      <label>Name<input data-name spellcheck="false" autocomplete="off" autocapitalize="off" placeholder="make_progress.star"></label>
      <div class="field-note" data-editor-note></div>
      <label>Source<textarea data-source spellcheck="false" autocomplete="off" autocapitalize="off" rows="16"></textarea></label>
      <pre class="parser-script-error" role="alert" data-error hidden></pre>
      <div class="parser-script-editor-actions"><button type="button" class="text-button" data-cancel>Cancel</button><button type="button" class="primary" data-save>Save and check</button></div>
    </div>
    <div class="parser-script-popover" role="menu" data-popover hidden></div>`;
  const q=s=>root.querySelector(s);
  const list=q('[data-list]'),note=q('[data-note]'),editor=q('[data-editor]'),popover=q('[data-popover]');
  const name=q('[data-name]'),source=q('[data-source]'),error=q('[data-error]');
  let scripts=[],editing=null,trigger=null;
  const path=n=>'/api/scripts/'+encodeURIComponent(n);

  function closePopover(){if(popover.hidden)return;popover.hidden=true;trigger?.setAttribute('aria-expanded','false');}
  function openPopover(button,actions){
    closePopover();trigger=button;button.setAttribute('aria-expanded','true');
    popover.replaceChildren(...actions.map(([label,run,danger])=>{const b=el('button','text-button'+(danger?' danger':''),label);b.type='button';b.setAttribute('role','menuitem');b.onclick=()=>{closePopover();run();};return b;}));
    button.closest('.parser-script').append(popover);popover.hidden=false;popover.querySelector('button')?.focus();
  }
  // Temporary popover: closes on an outside tap, Escape (without closing Settings) or after an action.
  document.addEventListener('pointerdown',event=>{if(!popover.hidden&&!popover.contains(event.target)&&event.target!==trigger)closePopover();});
  root.addEventListener('keydown',event=>{if(event.key==='Escape'&&!popover.hidden){event.preventDefault();event.stopPropagation();closePopover();trigger?.focus();}},true);

  function render(next){
    scripts=next;closePopover();popover.remove();
    list.replaceChildren(...scripts.map(script=>{
      const row=el('div','parser-script');row.setAttribute('role','listitem');row.dataset.script=script.name;
      const label=el('span','parser-script-name',script.name);label.title=script.name;row.append(label);
      if(script.builtin)row.append(el('span','parser-script-badge','Built-in'));
      else if(script.shipped)row.append(el('span','parser-script-badge','Modified'));
      if(script.error)row.append(el('span','parser-script-badge error','Error'));
      const toggle=el('label','parser-script-toggle');const box=el('input');box.type='checkbox';box.checked=script.enabled;
      box.setAttribute('aria-label',`Enable ${script.name}`);box.onchange=()=>setEnabled(script,box.checked,box);
      toggle.append(box,el('span','','On'));row.append(toggle);
      const edit=el('button','text-button','Edit');edit.type='button';edit.setAttribute('aria-label',`Edit ${script.name}`);edit.onclick=()=>open(script);row.append(edit);
      const actions=script.builtin?[]:script.shipped?[['Restore shipped version',()=>remove(script,'restore')]]:[['Delete…',()=>remove(script,'delete'),true]];
      if(actions.length){const more=el('button','text-button parser-script-more','⋯');more.type='button';more.setAttribute('aria-label',`More actions for ${script.name}`);more.setAttribute('aria-haspopup','menu');more.setAttribute('aria-expanded','false');more.onclick=()=>popover.hidden||trigger!==more?openPopover(more,actions):closePopover();row.append(more);}else row.append(el('span','parser-script-more-slot'));
      const wrap=el('div','parser-script-entry');wrap.dataset.scriptEntry=script.name;wrap.setAttribute('role','listitem');row.removeAttribute('role');wrap.append(row);
      if(script.hooks?.length){
        const hooks=el('ul','parser-script-hooks');
        for(const hook of script.hooks){
          const item=el('li');item.append(document.createTextNode(hook.hook==='progress'?'Tool progress · command: ':hook.hook==='tool_header'?'Tool header · title: ':'Formatter · '+hook.message_type+' · text: '),el('code','',hook.pattern));hooks.append(item);
        }
        wrap.append(hooks);
      }
      if(script.error)wrap.append(el('pre','parser-script-error',script.error));
      return wrap;
    }));
    if(!scripts.length)list.append(el('div','field-note','No scripts.'));
  }
  async function load(){
    note.textContent='';close();
    try{render((await api('/api/scripts')).scripts);}catch(e){list.replaceChildren();note.textContent='Cannot load scripts: '+e.message;}
  }
  async function setEnabled(script,enabled,box){
    box.disabled=true;
    try{render((await api(path(script.name)+'/enabled',{method:'PUT',body:JSON.stringify({enabled})})).scripts);note.textContent=`${script.name} ${enabled?'enabled':'disabled'}. Applies to future output and inbox deliveries.`;}
    catch(e){box.checked=!enabled;box.disabled=false;note.textContent='Not changed: '+e.message;}
  }
  async function remove(script,kind){
    if(kind==='delete'&&!confirm(`Delete ${script.name}? Its registered hooks stop running.`))return;
    try{render((await api(path(script.name),{method:'DELETE'})).scripts);note.textContent=kind==='restore'?`${script.name} restored to the shipped version.`:`${script.name} deleted.`;if(editing===script.name)close();}
    catch(e){note.textContent=(kind==='restore'?'Not restored: ':'Not deleted: ')+e.message;}
  }
  function open(script){
    editing=script?.name??null;editor.hidden=false;error.hidden=true;error.textContent='';
    q('[data-starter-label]').hidden=!!script;q('[data-starter]').value='progress';
    name.value=script?.name??'';name.readOnly=!!script;source.value=script?.source??TEMPLATE;
    q('[data-editor-note]').textContent=script?.builtin?'Saving makes this your version; it stops following Agentworks updates until you restore it.':script?'':'Names use lowercase letters, digits, - or _ and end in .star. Register one or more handlers with agent.register_progress or agent.register_formatter. New scripts are enabled when saved.';
    (script?source:name).focus();
  }
  function close(){editing=null;editor.hidden=true;error.hidden=true;}
  async function save(){
    const n=name.value.trim(),button=q('[data-save]');error.hidden=true;
    if(!n){error.textContent='Enter a name ending in .star.';error.hidden=false;name.focus();return;}
    if(!editing&&scripts.some(s=>s.name===n)){error.textContent=`${n} already exists; edit it instead.`;error.hidden=false;return;}
    button.disabled=true;
    try{render((await api(path(n),{method:'PUT',body:JSON.stringify({source:source.value})})).scripts);note.textContent=`${n} saved and checked.`;close();}
    catch(e){error.textContent=e.message;error.hidden=false;}
    finally{button.disabled=false;}
  }
  q('[data-starter]').onchange=event=>{if(!editing)source.value=TEMPLATES[event.target.value];};
  q('[data-new]').onclick=()=>open(null);
  q('[data-cancel]').onclick=()=>{close();note.textContent='';};
  q('[data-save]').onclick=save;
  return {load};
}
