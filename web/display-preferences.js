import {localState as localStorage} from './hosting.js';
// Browser-local display preferences for code/literal blocks.
// Raw block text is captured before decoration and is the only source for Copy.
const KEY='agentworks-display-preferences';
const CHOICES={
  wrap:[['auto','Wrap on narrow screens'],['wrap','Always wrap'],['scroll','Never wrap (scroll)']],
  highlight:[['on','Highlight supported languages'],['off','Off (plain monospace)']],
  prose:[['detect','Show as plain text'],['code','Keep as code']],
};
const LABELS={wrap:'Long lines in code blocks',highlight:'Syntax highlighting',prose:'Prose in unlabeled code blocks'};
const HIGHLIGHT_LIMIT=60000,TOKEN_LIMIT=20000;

export function loadDisplayPreferences(storage=localStorage){
  let saved={};try{saved=JSON.parse(storage.getItem(KEY))||{};}catch{}
  return Object.fromEntries(Object.entries(CHOICES).map(([key,options])=>[key,options.some(([value])=>value===saved[key])?saved[key]:options[0][0]]));
}

// Unlabeled blocks that read like sentences rather than code.
export function looksLikeProse(text){
  const trimmed=text.trim();if(trimmed.length<40)return false;
  const words=trimmed.match(/[A-Za-z][a-z']+/g)||[];
  const symbols=(trimmed.match(/[{}()[\];=<>$\\|&*`^~]/g)||[]).length;
  const lines=trimmed.split('\n'),indented=lines.filter(line=>/^( {2,}|\t)/.test(line)).length;
  return words.length>=8&&symbols/trimmed.length<0.02&&/[.!?,]\s/.test(trimmed+' ')&&indented<=lines.length/3;
}

// Conservative tokenizer: comments, strings, numbers and keywords for known languages only.
const keywordSets={
  js:'async await break case catch class const continue default delete do else export extends false finally for from function if import in instanceof let new null of return static super switch this throw true try typeof undefined var void while yield interface type enum implements',
  rust:'as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait true type unsafe use where while Some None Ok Err',
  python:'and as assert async await break class continue def del elif else except False finally for from global if import in is lambda None nonlocal not or pass raise return True try while with yield',
  sh:'if then else elif fi for while until do done case esac function in return local export set unset echo cd exit',
  go:'break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select struct switch type var nil true false',
  c:'auto break case char class const continue default do double else enum extern false float for goto if int long namespace new nullptr private protected public return short signed sizeof static struct switch template this true typedef union unsigned using virtual void volatile while bool',
  sql:'select from where insert into values update set delete create table index join left right inner outer on group by order having limit as and or not null primary key',
  json:'true false null',
};
const aliases={javascript:'js',jsx:'js',ts:'js',typescript:'js',tsx:'js',mjs:'js',cjs:'js',rs:'rust',py:'python',bash:'sh',shell:'sh',zsh:'sh',golang:'go',cpp:'c','c++':'c',h:'c',hpp:'c',cc:'c',java:'c',cs:'c',csharp:'c',kotlin:'c',kt:'c',swift:'c',toml:'conf',yaml:'conf',yml:'conf',ini:'conf'};
export function supportedLanguage(language){
  const id=aliases[language?.toLowerCase()]||language?.toLowerCase();
  return keywordSets[id]||id==='conf'?id:null;
}
function grammar(id){
  return {
    keywords:new Set((keywordSets[id]||'').split(' ').filter(Boolean)),caseless:id==='sql',
    comment:['python','sh','conf'].includes(id)?/#[^\n]*/y:id==='sql'?/--[^\n]*/y:id==='json'?null:/\/\/[^\n]*|\/\*[\s\S]*?(?:\*\/|$)/y,
    backtick:['js','go','sh'].includes(id),
  };
}
const STRING=/"(?:[^"\\\n]|\\.)*"?|'(?:[^'\\\n]|\\.)*'?/y,BACKTICK=/`(?:[^`\\]|\\[\s\S])*`?/y,NUMBER=/\b(?:0x[\da-f_]+|\d[\d_]*(?:\.\d[\d_]*)?(?:e[+-]?\d+)?)\b/iy,WORD=/[A-Za-z_][\w$]*/y;
export function tokenize(text,id){
  const g=grammar(id),out=[];let plain='',i=0;
  const flush=()=>{if(plain){out.push(['',plain]);plain='';}};
  const take=(regex,kind)=>{if(!regex)return false;regex.lastIndex=i;const m=regex.exec(text);if(!m||!m[0])return false;flush();out.push([kind,m[0]]);i+=m[0].length;return true;};
  while(i<text.length){
    if(out.length>TOKEN_LIMIT){plain+=text.slice(i);break;}
    if(take(g.comment,'comment')||take(STRING,'string')||(g.backtick&&take(BACKTICK,'string'))||take(NUMBER,'number'))continue;
    WORD.lastIndex=i;const word=WORD.exec(text);
    if(word){i+=word[0].length;if(g.keywords.has(g.caseless?word[0].toLowerCase():word[0])){flush();out.push(['keyword',word[0]]);}else plain+=word[0];continue;}
    plain+=text[i++];
  }
  flush();return out;
}

export function setupDisplayPreferences({root=document,storage=localStorage}={}){
  let preferences=loadDisplayPreferences(storage);
  const overrides=new Map();
  const apply=()=>{root.body.dataset.codeWrap=preferences.wrap;for(const block of root.querySelectorAll('.code-block'))renderBlock(block);};

  function renderBlock(block){
    const {raw,language}=block.codeState,code=block.querySelector('pre > code'),toggle=block.querySelector('.code-mode');
    const override=overrides.get(block.dataset.blockKey);
    const plain=override?override==='plain':!language&&preferences.prose==='detect'&&looksLikeProse(raw);
    block.classList.toggle('plain',plain);
    toggle.textContent=plain?'Show as code':'Show as text';toggle.setAttribute('aria-pressed',String(plain));
    const id=!plain&&preferences.highlight==='on'&&raw.length<=HIGHLIGHT_LIMIT?supportedLanguage(language):null;
    block.classList.toggle('highlighted',!!id);
    if(!id){if(code.childElementCount||code.textContent!==raw)code.textContent=raw;return;}
    const fragment=root.createDocumentFragment();
    for(const [kind,text]of tokenize(raw,id)){if(!kind){fragment.append(text);continue;}const span=root.createElement('span');span.className='tok-'+kind;span.textContent=text;fragment.append(span);}
    code.replaceChildren(fragment);
  }

  async function copy(text){
    try{await navigator.clipboard.writeText(text);return true;}catch{}
    // Clipboard API needs a secure context; remote HTTP origins try the legacy command.
    const area=root.createElement('textarea');area.value=text;area.setAttribute('readonly','');area.style.cssText='position:fixed;opacity:0;top:0;left:0';
    root.body.append(area);area.select();let ok=false;try{ok=root.execCommand('copy');}catch{}area.remove();return ok;
  }

  // Wrap each <pre> in a labeled block; key identifies the block across Markdown re-renders.
  function enhance(container,key='block',{language:fallbackLanguage=''}={}){
    [...container.querySelectorAll('pre')].forEach((pre,index)=>{
      if(pre.closest('.code-block'))return;
      let code=pre.querySelector(':scope > code');
      if(!code){code=root.createElement('code');code.append(...pre.childNodes);pre.append(code);}
      const raw=code.textContent,language=/(?:^|\s)language-(\S+)/.exec(code.className)?.[1]||fallbackLanguage;
      const block=root.createElement('figure');block.className='code-block';block.dataset.blockKey=key+':'+index;block.codeState={raw,language};
      const bar=root.createElement('figcaption'),name=root.createElement('span');name.className='code-language';name.textContent=language||'text';
      const actions=root.createElement('span');actions.className='code-actions';
      const mode=root.createElement('button');mode.type='button';mode.className='code-mode';
      mode.onclick=()=>{overrides.set(block.dataset.blockKey,block.classList.contains('plain')?'code':'plain');renderBlock(block);};
      const copyButton=root.createElement('button');copyButton.type='button';copyButton.className='code-copy';copyButton.textContent='Copy';
      copyButton.setAttribute('aria-label',`Copy ${language||'text'} block`);
      const status=root.createElement('span');status.className='code-copy-status';status.setAttribute('role','status');
      copyButton.onclick=async()=>{
        const ok=await copy(block.codeState.raw);clearTimeout(copyButton.timer);
        if(ok){status.textContent='Copied';copyButton.textContent='Copied';copyButton.timer=setTimeout(()=>{copyButton.textContent='Copy';status.textContent='';},2000);return;}
        // Visible fallback: exact raw text in a selected read-only field (DOM selections can drop the final newline).
        let field=block.querySelector('.code-copy-fallback');
        if(!field){field=root.createElement('textarea');field.className='code-copy-fallback';field.readOnly=true;field.spellcheck=false;field.setAttribute('aria-label',`Raw ${language||'text'} block for copying`);status.before(field);}
        field.value=block.codeState.raw;field.rows=Math.min(8,block.codeState.raw.split('\n').length);field.focus();field.select();
        status.textContent='Copy unavailable here. Raw text selected below; press Ctrl+C or ⌘C.';
      };
      actions.append(mode,copyButton);bar.append(name,actions);
      pre.replaceWith(block);block.append(bar,pre,status);pre.tabIndex=0;pre.setAttribute('aria-label',(language||'Text')+' block');renderBlock(block);
    });
  }

  function mount(fieldset){
    fieldset.replaceChildren();
    const legend=root.createElement('legend');legend.textContent='Display · this browser';fieldset.append(legend);
    const note=root.createElement('p');note.className='field-note';note.id='display-preferences-note';note.setAttribute('role','status');
    note.textContent='Applied immediately and remembered in this browser. Copy always uses the original block text.';
    for(const [key,options]of Object.entries(CHOICES)){
      const id='display-'+key,label=root.createElement('label'),select=root.createElement('select');
      label.htmlFor=id;label.textContent=LABELS[key];select.id=id;select.setAttribute('aria-describedby',note.id);
      for(const [value,text]of options){const option=root.createElement('option');option.value=value;option.textContent=text;select.append(option);}
      select.value=preferences[key];
      select.onchange=()=>{
        preferences={...preferences,[key]:select.value};
        try{storage.setItem(KEY,JSON.stringify(preferences));note.textContent=`${LABELS[key]} saved in this browser.`;}
        catch{note.textContent=`${LABELS[key]} applied, but this browser could not remember it.`;}
        apply();
      };
      const row=root.createElement('div');row.className='display-field';row.append(label,select);fieldset.append(row);
    }
    fieldset.append(note);
  }

  apply();
  return {enhance,mount,copy,preferences:()=>({...preferences})};
}
