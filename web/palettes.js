// Command/action palette and session-activation palette. Ranking is pure and side-effect free so it can be
// tested without a browser; setupPalettes only wires the dialog, keyboard shortcuts and existing UI actions.
const BOUNDARY=/[\s/\\\-_.:,;·()[\]{}@#]/;
const START_BONUS=12,WORD_BONUS=9,CONSECUTIVE_BONUS=7,GAP_PENALTY=.4;
const ACTION_WEIGHTS={label:1,search:.6,hint:.35,detail:.5,group:.25};
const SESSION_WEIGHTS={label:1.4,search:.9,detail:.7,group:.2};

// Score one term against one field, or null when the term is not a subsequence of it. Optimal alignment is
// found with a small dynamic program: word starts, consecutive runs and earlier matches earn the score.
export function fuzzyScore(term,text,original=text){
  if(!term)return {score:0,positions:[]};
  const needle=term.toLowerCase(),hay=String(text??'').toLowerCase(),source=String(original??text??'');
  const width=needle.length,length=hay.length;
  if(!length||width>length)return null;
  const score=Array.from({length:width},()=>new Float64Array(length).fill(-Infinity));
  const streak=Array.from({length:width},()=>new Int32Array(length));
  let best=-Infinity,bestEnd=-1;
  for(let i=0;i<width;i++){
    let run=-Infinity; // best score of row i-1 strictly before the current column
    for(let j=0;j<length;j++){
      if(needle[i]===hay[j]){
        const diagonal=i>0&&j>0?score[i-1][j-1]:-Infinity;
        const diagonalStreak=i>0&&j>0?streak[i-1][j-1]:0;
        let base,runLength;
        if(i===0){base=0;runLength=1;}
        else{
          const consecutive=diagonal>=run;
          base=consecutive?diagonal:run;
          runLength=consecutive?diagonalStreak+1:1;
        }
        if(base>-Infinity){
          const boundary=j===0?START_BONUS:(BOUNDARY.test(hay[j-1])||(/[a-z0-9]/.test(source[j-1]??'')&&/[A-Z]/.test(source[j]??''))?WORD_BONUS:0);
          const value=base+boundary+(runLength>1?(runLength-1)*CONSECUTIVE_BONUS:0);
          score[i][j]=value;streak[i][j]=runLength;
          if(value>best){best=value;bestEnd=j;}
        }
      }
      if(i>0&&j<length-1&&score[i-1][j]>run)run=score[i-1][j];
    }
  }
  if(bestEnd<0)return null;
  const positions=new Array(width);
  let end=bestEnd;
  for(let i=width-1;i>=0;i--){
    positions[i]=end;
    if(i===0)break;
    if(streak[i][end]>1){end--;continue;}
    let pick=-1,pickScore=-Infinity;
    for(let k=end-1;k>=0;k--)if(score[i-1][k]>pickScore){pickScore=score[i-1][k];pick=k;}
    if(pick<0)return null;
    end=pick;
  }
  const gaps=Math.max(0,bestEnd-positions[0]-(width-1));
  return {score:best-GAP_PENALTY*gaps,positions};
}

// Rank items against a fuzzy query. Every whitespace-separated term must match at least one field (AND), so
// "alpha api" finds a session whose title and directory both contribute. Input items are never mutated.
export function rankItems(query,items,{fields=['label','search','hint','detail','group'],weights=ACTION_WEIGHTS,limit=200}={}){
  const terms=String(query??'').trim().toLowerCase().split(/\s+/).filter(Boolean);
  const ranked=[];
  for(let index=0;index<items.length;index++){
    const item=items[index];
    let total=0,hits=0;
    const matches={};
    for(const term of terms){
      let best=null;
      for(const field of fields){
        const text=item[field];
        if(!text)continue;
        const result=fuzzyScore(term,text);
        if(!result)continue;
        const weighted=result.score*(weights[field]??1);
        if(!best||weighted>best.score)best={score:weighted,field,positions:result.positions};
      }
      if(!best)break;
      total+=best.score;matches[best.field]=best.positions;hits++;
    }
    if(hits===terms.length)ranked.push({...item,score:total,matches,index});
  }
  ranked.sort((a,b)=>b.score-a.score||a.index-b.index);
  return ranked.slice(0,limit);
}

// Advertised harness commands use the same text the composer shows, so a palette choice matches what is sent.
export function commandText(command){
  const name=typeof command==='string'?command:command?.name||'';
  return name.startsWith('$')?name:'/'+name.replace(/^\//,'');
}
export function pathTail(path,segments=1){
  return String(path??'').replaceAll('\\','/').split('/').filter(Boolean).slice(-segments).join('/');
}
export function statusLabel(status){
  return {running:'Running turn',starting:'Starting',resuming:'Resuming',configuring:'Configuring',cancelling:'Cancelling',
    waiting:'Waiting for approval',idle:'Ready',suspended:'Suspended',error:'Error',closed:'Closed'}[status]||status||'Unknown';
}

export function sessionItems(sessions,{labels={},hosts=[],projects=[],activeId=null}={}){
  const list=sessions||[];
  const hostName=id=>hosts.find(h=>h.id===id)?.name||id||'Local';
  const projectName=id=>projects.find(p=>p.id===id)?.name||null;
  const titles=new Map();
  for(const session of list)titles.set(session.title,(titles.get(session.title)||0)+1);
  const items=list.map(session=>{
    const project=projectName(session.projectId),host=hostName(session.hostId);
    // Equal titles need more of the path to stay distinguishable in the list.
    const directory=pathTail(session.cwd,(titles.get(session.title)||0)>1?3:1);
    const detail=[session.automation?'Automation':null,labels[session.provider]||session.provider,project,host,directory].filter(Boolean).join(' · ')+' · '+statusLabel(session.status);
    return {id:session.id,label:session.title,group:session.id===activeId?'Current session':project||'Ungrouped',detail,hint:session.cwd,
      // Search the tail of the path: the shared prefix of every session would otherwise match anything.
      search:[session.title,project,host,pathTail(session.cwd,3),labels[session.provider]||session.provider,statusLabel(session.status)].filter(Boolean).join(' '),
      status:session.status,current:session.id===activeId,session};
  });
  // Unprompted lists keep running work visible first; the session API already returns newest first.
  const busy=items.filter(item=>['running','waiting','starting','resuming','configuring','cancelling'].includes(item.status));
  return busy.length?[...busy,...items.filter(item=>!busy.includes(item))]:items;
}

export function sessionRanking(query,items,options={}){
  return rankItems(query,items,{fields:['label','search','detail','group'],weights:SESSION_WEIGHTS,...options});
}

function readiness(hasSession,connected,status){
  if(!hasSession)return {disabled:true,reason:'No session selected'};
  if(!connected)return {disabled:true,reason:'Not connected to this session yet'};
  if(status!=='idle')return {disabled:true,reason:'Available when the session is ready'};
  return {disabled:false,reason:''};
}

// Existing UI actions and advertised harness commands, each with its current disabled/readiness state.
export function actionItems(state={},handlers={}){
  const {active=null,status='starting',connected=false,commands=[],views={}}=state;
  const hasSession=!!active,ready=readiness(hasSession,connected,status);
  const items=[];
  const add=(id,label,group,hint,run,state={})=>items.push({
    id,label,group,hint,search:[label,group,hint].filter(Boolean).join(' '),disabled:false,reason:'',run,...state,
  });
  add('new-session','New session','Create','Open the new session dialog',()=>handlers.newSession?.());
  add('import-session','Resume existing session','Create','Continue a conversation saved on this host',()=>handlers.importSession?.());
  add('new-project','New project','Create','Group related sessions',()=>handlers.newProject?.());
  if(views.files)add('view-files','Project files','Go to','Browse the selected project directory',views.files);
  if(views.automations)add('view-automations','Automations','Go to','Saved repeatable instructions',views.automations);
  add('settings','Settings','Go to','Allowed roots and default models',()=>handlers.settings?.());
  if(hasSession){
    add('session-agents','Agents and inbox','Current session','Access, messages and control activity',()=>handlers.agents?.());
    add('session-workspace','Workspace info','Current session','Directory, branch and commit',()=>handlers.workspaceInfo?.());
    if(['suspended','error'].includes(status))
      add('session-resume','Resume session','Current session','Ask the harness to resume its saved context',
        ()=>handlers.resume?.(),connected?{}:{disabled:true,reason:'Not connected to this session yet'});
    const stopping=connected&&['running','waiting'].includes(status);
    add('session-stop','Stop turn','Current session','Interrupt the running turn',()=>handlers.stop?.(),
      stopping?{}:status==='cancelling'?{disabled:true,reason:'Already cancelling'}
        :['running','waiting'].includes(status)?{disabled:true,reason:'Not connected to this session yet'}
          :{disabled:true,reason:'No turn is running'});
    if(state.usageAvailable)
      add('session-usage','Usage','Current session','Ask this harness for reported usage',()=>handlers.usage?.(),ready);
    add('session-delete','Delete session','Current session','Stops the agent; files and branches remain',()=>handlers.deleteSession?.());
  }
  for(const command of commands||[]){
    const text=commandText(command);
    items.push({id:'command:'+text,label:text,group:'Harness commands',hint:command.description||'Advertised by this harness',
      search:[text,command.description].filter(Boolean).join(' '),command:text,run:()=>handlers.insertCommand?.(text),...ready});
  }
  return items;
}

// Palette dialog: Ctrl/Cmd+K actions, Ctrl/Cmd+P sessions, Escape closes, arrows move the active option.
export function setupPalettes({$,state=()=>({}),handlers={},selectSession,labels={},hosts=[],projects=[],sessions=[],onOpen,fallbackFocus}={}){
  const dialog=$('palette-dialog'),input=$('palette-input'),list=$('palette-list'),status=$('palette-status');
  if(!dialog||!input||!list)return null;
  const tabs=[$('palette-mode-actions'),$('palette-mode-sessions')].filter(Boolean);
  const placeholders={actions:'Search actions and harness commands…',sessions:'Search session titles, projects, hosts or directories…'};
  let mode='actions',ranked=[],active=0,restoreFocus=null;
  const list0=()=>state().sessions||sessions,hosts0=()=>state().hosts||hosts,projects0=()=>state().projects||projects;

  function build(){
    const current=state();
    return mode==='sessions'?sessionItems(list0(),{labels,hosts:hosts0(),projects:projects0(),activeId:current.active?.id}):actionItems(current,handlers);
  }
  function highlighted(parent,text,positions){
    const source=String(text??''),set=positions&&positions.length?new Set(positions):null;
    if(!set){parent.textContent=source;return;}
    let cursor=0;
    source.split('').forEach((char,index)=>{
      if(!set.has(index))return;
      if(index>cursor)parent.append(document.createTextNode(source.slice(cursor,index)));
      const mark=document.createElement('mark');mark.textContent=char;parent.append(mark);
      cursor=index+1;
    });
    if(cursor<source.length)parent.append(document.createTextNode(source.slice(cursor)));
  }
  function render(){
    const query=input.value.trim();
    ranked=mode==='sessions'?sessionRanking(query,build()):rankItems(query,build());
    active=ranked.length?Math.max(0,Math.min(active,ranked.length-1)):0;
    list.replaceChildren();
    if(!ranked.length){
      const empty=document.createElement('p');empty.className='palette-empty';
      empty.textContent=query?'No matches for this search.':'Nothing to show yet.';list.append(empty);
    }
    ranked.forEach((item,index)=>{
      const option=document.createElement('div');
      option.className='palette-option'+(item.disabled?' disabled':'');
      option.id='palette-option-'+index;option.dataset.index=String(index);
      option.setAttribute('role','option');
      option.setAttribute('aria-selected',String(index===active));
      if(item.disabled)option.setAttribute('aria-disabled','true');
      const label=document.createElement('span');label.className='palette-option-label';
      highlighted(label,item.label,item.matches?.label);
      const group=document.createElement('span');group.className='palette-option-group';group.textContent=item.group||'';
      option.append(label,group);
      if(item.detail){
        const detail=document.createElement('span');detail.className='palette-option-detail';
        highlighted(detail,item.detail,item.matches?.detail);
        option.append(detail);
      }
      if(item.disabled&&item.reason){
        const reason=document.createElement('span');reason.className='palette-option-reason';reason.textContent=item.reason;
        option.append(reason);
      }
      if(item.hint){
        option.title=item.hint;
        const hint=document.createElement('span');hint.className='palette-option-hint';hint.textContent=item.hint;
        option.append(hint);
      }
      option.addEventListener('click',()=>{active=index;activate();});
      option.addEventListener('mousemove',()=>{if(active===index)return;active=index;paint();});
      list.append(option);
    });
    const chosen=ranked[active];
    status.textContent=ranked.length?ranked.length+(ranked.length===1?' result':' results')+(chosen?.disabled?' · selected entry unavailable':''):'No results';
    paint();
  }
  function paint(){
    const options=list.querySelectorAll('[role=option]');
    for(const option of options)option.setAttribute('aria-selected',String(Number(option.dataset.index)===active));
    const current=options[active];
    if(!current){input.removeAttribute('aria-activedescendant');return;}
    input.setAttribute('aria-activedescendant',current.id);
    current.scrollIntoView({block:'nearest'});
  }
  function move(step){
    if(!ranked.length)return;
    active=(active+step+ranked.length)%ranked.length;
    paint();
  }
  function activate(){
    const item=ranked[active];
    if(!item)return;
    if(item.disabled){status.textContent=item.reason||'This entry is unavailable right now.';return;}
    close();
    if(item.session)selectSession?.(item.session);
    else item.run?.();
  }
  function show(next='actions'){
    if(document.querySelector('dialog[open]:not(#palette-dialog)'))return false;
    mode=next;input.value='';active=0;restoreFocus=document.activeElement;
    for(const tab of tabs)tab.setAttribute('aria-pressed',String(tab.id==='palette-mode-'+mode));
    $('palette-title').textContent=mode==='sessions'?'Switch session':'Command palette';
    input.placeholder=placeholders[mode];
    if(!dialog.open)dialog.showModal();
    render();input.focus();onOpen?.(mode);
    return true;
  }
  function close(){if(dialog.open)dialog.close();}
  function toggle(next){dialog.open&&mode===next?close():show(next);}

  dialog.addEventListener('close',()=>{
    ranked=[];active=0;input.removeAttribute('aria-activedescendant');
    const previous=restoreFocus;restoreFocus=null;
    const usable=node=>node&&node.isConnected&&node!==document.body&&typeof node.focus==='function';
    const target=usable(previous)?previous:(usable(fallbackFocus?.())?fallbackFocus():null);
    target?.focus();
  });
  $('dismiss-palette').onclick=close;
  for(const tab of tabs)tab.onclick=()=>show(tab.id==='palette-mode-sessions'?'sessions':'actions');
  input.addEventListener('input',()=>{active=0;render();});
  input.addEventListener('keydown',event=>{
    if(event.isComposing)return;
    switch(event.key){
      case 'ArrowDown':case 'ArrowRight':event.preventDefault();move(1);break;
      case 'ArrowUp':case 'ArrowLeft':event.preventDefault();move(-1);break;
      case 'Home':if(input.value){event.preventDefault();active=0;paint();}break;
      case 'End':if(input.value){event.preventDefault();active=Math.max(0,ranked.length-1);paint();}break;
      case 'PageDown':event.preventDefault();move(8);break;
      case 'PageUp':event.preventDefault();move(-8);break;
      case 'Enter':event.preventDefault();activate();break;
      case 'Escape':event.preventDefault();close();break;
      case 'Tab':{
        event.preventDefault();
        show(mode==='actions'?'sessions':'actions');
        break;
      }
      default:break;
    }
  });
  $('palette-open')?.addEventListener('click',()=>show('actions'));
  $('palette-search')?.addEventListener('click',()=>show('actions'));
  document.addEventListener('keydown',event=>{
    if(event.isComposing||event.altKey||event.shiftKey||event.defaultPrevented)return;
    if(!event.ctrlKey&&!event.metaKey)return;
    const key=event.key.toLowerCase();
    if(key!=='k'&&key!=='p')return;
    // Another modal owns the keyboard; never steal Ctrl/Cmd+K or Ctrl/Cmd+P from it.
    if(document.querySelector('dialog[open]:not(#palette-dialog)'))return;
    event.preventDefault();
    toggle(key==='k'?'actions':'sessions');
  });
  return {show,close,mode:()=>mode,isOpen:()=>dialog.open};
}