import {localState as localStorage} from './hosting.js';
// Share one PR request between the dashboard and navigation reminders while the app tab is visible.
export function setupGithubBrowser({api,root,openSession,selectSession,openSettings}){
  const el=(tag,text,cls)=>{const n=document.createElement(tag);if(text!==undefined)n.textContent=text;if(cls)n.className=cls;return n;};
  let active=false,busy=false,timer=null,data=null;
  const rows=new Map(),collapsed=new Set();
  try{for(const key of JSON.parse(localStorage.getItem('agentworks-github-collapsed')||'[]'))collapsed.add(key);}catch{}
  function group(key,title){const details=el('details',undefined,'github-browser-group');details.open=!collapsed.has(key);details.append(el('summary',title));details.ontoggle=()=>{if(!details.isConnected)return;if(details.open)collapsed.delete(key);else collapsed.add(key);try{localStorage.setItem('agentworks-github-collapsed',JSON.stringify([...collapsed]));}catch{}};return details;}
  function destination(value){try{const u=new URL(value);return u.protocol==='https:'&&!u.username&&!u.password?u.href:null;}catch{return null;}}
  const chooser=el('dialog');chooser.id='github-session-dialog';chooser.setAttribute('aria-labelledby','github-session-title');document.body.append(chooser);
  let pivotGeneration=0;
  const closePivot=()=>{pivotGeneration++;chooser.close();};
  chooser.addEventListener('close',()=>{pivotGeneration++;});
  async function pivot(item){
    const version=++pivotGeneration;
    const heading=el('h2',`Open PR #${item.pr.number} in a session`);heading.id='github-session-title';
    const content=el('div','Finding associated sessions…','field-note'),error=el('p',undefined,'error-text');error.role='alert';
    const footer=el('div',undefined,'dialog-footer'),cancel=el('button','Cancel','text-button'),create=el('button','New session…','primary');
    cancel.onclick=closePivot;create.onclick=()=>{closePivot();openSession(item);};footer.append(cancel,create);
    chooser.replaceChildren(heading,el('p',`${item.repository.host}/${item.repository.owner}/${item.repository.name}`,'field-note'),content,error,footer);chooser.showModal();
    try{
      const query=new URLSearchParams({host:item.repository.host,owner:item.repository.owner,repo:item.repository.name,number:String(item.pr.number)});
      const result=await api('/api/github/sessions?'+query);
      if(version!==pivotGeneration||!chooser.open)return;
      if(!result.sessions.length){closePivot();openSession(item);return;}
      content.className='github-session-list';content.replaceChildren();
      const activeList=el('div',undefined,'github-session-active github-session-choices');
      const archived=result.sessions.filter(match=>match.session.archived);
      if(!result.sessions.some(match=>!match.session.archived))activeList.append(el('p','No active sessions for this PR.','field-note'));
      content.append(activeList);
      let archivedList=null,archivedSection=null;
      if(archived.length){
        archivedSection=el('details',undefined,'github-session-archived');
        archivedSection.append(el('summary',`Archived sessions (${archived.length})`),el('p','Open a saved conversation without unarchiving it.','field-note'));
        archivedList=el('div',undefined,'github-session-choices');archivedSection.append(archivedList);content.append(archivedSection);
      }
      let opening=false;
      for(const match of result.sessions){
        const s=match.session,button=el('button',undefined,'github-session-choice');button.type='button';
        button.append(el('strong',s.title),el('small',`${match.projectName||'Ungrouped'} · ${match.hostName} · ${s.provider} · ${s.status}`),el('small',s.cwd));
        button.onclick=async()=>{
          if(opening)return;opening=true;error.textContent='';
          for(const b of chooser.querySelectorAll('button'))b.disabled=true;
          try{
            const session=await api(`/api/sessions/${encodeURIComponent(s.id)}?view=meta`);
            if(version!==pivotGeneration||!chooser.open)return;
            if(session.archived&&!s.archived)throw new Error("This session has been archived. Reopen the chooser to find it under Archived sessions.");
            closePivot();selectSession(session);
          }catch(e){if(version===pivotGeneration)error.textContent=e.message;}
          finally{opening=false;if(version===pivotGeneration)for(const b of chooser.querySelectorAll('button'))b.disabled=false;}
        };
        (s.archived?archivedList:activeList).append(button);
      }
      (activeList.querySelector('button')||archivedSection?.querySelector('summary'))?.focus();
    }catch(e){if(version===pivotGeneration&&chooser.open){content.textContent='Could not load associated sessions.';error.textContent=e.message;}}
  }
  const columns=[['title','Pull request'],['repository','Repository'],['author','Author'],['included','Included because'],['review','Review'],['ci','CI'],['updated','Updated'],['created','Created']];
  const filters={title:'',repository:'',author:'',included:'',review:'',ci:'',updatedFrom:'',updatedThrough:'',createdFrom:'',createdThrough:'',draftReview:''};
  let sort={field:'updated',direction:'desc'},groupRepos=false,filtersOpen=false;
  try{const saved=JSON.parse(localStorage.getItem('agentworks-github-table')||'null');if(saved&&[...columns.map(c=>c[0]),'number'].includes(saved.field)&&['asc','desc'].includes(saved.direction)){sort={field:saved.field,direction:saved.direction};groupRepos=!!saved.groupRepos;}}catch{}
  const saveTable=()=>{try{localStorage.setItem('agentworks-github-table',JSON.stringify({...sort,groupRepos}));}catch{}};
  const review=item=>item.pr.draft?'Draft':({APPROVED:'Approved',REVIEW_REQUIRED:'In review',CHANGES_REQUESTED:'Changes requested',NONE:'No review',UNKNOWN:'Unknown'}[item.pr.reviewSummary?.state||item.pr.reviewDecision]||'Unknown');
  const ci=item=>({SUCCESS:'Passing',FAILURE:'Failing',ERROR:'Failing',PENDING:'Pending',EXPECTED:'Waiting'}[item.pr.checks?.state]||'Unknown');
  const included=item=>[item.authored?'Created by me':'',item.assigned?'Assigned to me':'',item.reviewRequested?'Review requested':''].filter(Boolean).join(' · ');
  const timestamp=value=>{const n=Date.parse(value);return Number.isFinite(n)?n:null;};
  function value(item,field){
    return ({title:item.pr.title||'',repository:`${item.repository.host}/${item.repository.owner}/${item.repository.name}`,author:item.pr.author||'Unknown',included:included(item),review:review(item),ci:ci(item),number:item.pr.number,updated:timestamp(item.pr.updatedAt),created:timestamp(item.pr.createdAt)})[field];
  }
  const pendingAccounts=item=>Array.isArray(item.pendingReviews?.accounts)?item.pendingReviews.accounts:[];
  const draftState=item=>pendingAccounts(item).length?'pending':item.pendingReviews?.complete===true&&!item.pendingReviews?.stale&&!item.stale?'none':'unknown';
  function draftAlert(item){
    const accounts=pendingAccounts(item);if(!accounts.length)return null;
    const stale=item.stale||item.pendingReviews?.stale,count=accounts.reduce((sum,a)=>sum+(Number.isSafeInteger(a.commentCount)&&a.commentCount>0?a.commentCount:0),0);
    const text=`Unsubmitted review${count?` · ${count}${item.pendingReviews?.complete!==true&&!stale?'+':''} draft ${count===1?'comment':'comments'}`:''}${stale?' · last seen':''}`;
    const href=destination(item.pr.url),alert=el(href?'a':'span',undefined,'github-draft-review-alert');
    const mark=el('span','!','github-draft-review-mark');mark.setAttribute('aria-hidden','true');alert.append(mark,el('strong',text));
    alert.title=accounts.map(a=>`${a.login}: ${a.commentCount} draft ${a.commentCount===1?'comment':'comments'}`).join(' · ')+(stale?' · Last observed; current status unavailable':'');
    if(href){const url=new URL(href);url.pathname=url.pathname.replace(/\/$/,'')+'/files';url.search='';url.hash='';alert.href=url.href;alert.target='_blank';alert.rel='noopener noreferrer';}
    return alert;
  }
  function draftSummary(){
    const pending=data.hosts.flatMap(host=>host.rows.filter(item=>draftState(item)==='pending').map(item=>({host,item})));
    if(!pending.length)return null;
    const box=el('div',undefined,'github-draft-review-summary');box.role='status';const mark=el('span','!','github-draft-review-mark');mark.setAttribute('aria-hidden','true');
    box.append(mark,el('strong',`${pending.length} ${pending.length===1?'PR has':'PRs have'} unsubmitted reviews${pending.some(({item})=>item.stale||item.pendingReviews?.stale)?' · includes last-seen status':''}`));
    const show=el('button','Show these PRs','text-button');show.onclick=()=>{
      for(const key in filters)filters[key]='';filters.draftReview='pending';
      for(const {host,item} of pending){collapsed.delete('host:'+host.host);collapsed.delete(`${host.host}/${item.repository.owner}/${item.repository.name}`);}
      try{localStorage.setItem('agentworks-github-collapsed',JSON.stringify([...collapsed]));}catch{}
      render();
    };box.append(show);return box;
  }
  function matches(item){
    for(const field of ['title','repository','author']){
      const text=field==='title'?`${item.pr.number} ${item.pr.title||''} ${item.pr.headBranch||''}`:value(item,field);
      if(!text.toLowerCase().includes(filters[field].trim().toLowerCase()))return false;
    }
    if(filters.included&&!item[filters.included])return false;
    if(filters.draftReview&&draftState(item)!==filters.draftReview)return false;
    if(filters.review&&review(item)!==filters.review)return false;
    if(filters.ci&&ci(item)!==filters.ci)return false;
    for(const field of ['updated','created']){
      const date=value(item,field),from=timestamp(filters[field+'From']),through=timestamp(filters[field+'Through']);
      if((from!==null&&(date===null||date<from))||(through!==null&&(date===null||date>=through+86400000)))return false;
    }
    return true;
  }
  function sorted(items){return [...items].sort((a,b)=>{
    const av=value(a,sort.field),bv=value(b,sort.field);
    // Undated records remain last in either direction.
    if(av===null||bv===null){if(av!==bv)return av===null?1:-1;}
    const comparison=typeof av==='number'&&typeof bv==='number'?av-bv:String(av??'').localeCompare(String(bv??''),undefined,{numeric:true,sensitivity:'base'});
    return comparison*(sort.direction==='asc'?1:-1)||value(a,'repository').localeCompare(value(b,'repository'))||a.pr.number-b.pr.number;
  });}
  function setSort(field){sort=sort.field===field?{field,direction:sort.direction==='asc'?'desc':'asc'}:{field,direction:['updated','created'].includes(field)?'desc':'asc'};saveTable();render();}
  function controls(){
    const area=el('div',undefined,'github-table-controls'),bar=el('div',undefined,'github-table-toolbar');
    const label=el('label','Sort by '),select=el('select');select.setAttribute('aria-label','Sort PRs by');select.dataset.githubControl='sort';
    for(const [key,title] of [...columns,['number','PR number']]){const option=el('option',title);option.value=key;select.append(option);}select.value=sort.field;
    select.onchange=()=>{const field=select.value;sort={field,direction:['updated','created'].includes(field)?'desc':'asc'};saveTable();render();};label.append(select);
    const direction=el('button',sort.direction==='desc'?'Descending ↓':'Ascending ↑','text-button');direction.dataset.githubControl='direction';direction.setAttribute('aria-label','Reverse PR sort order');direction.onclick=()=>{sort.direction=sort.direction==='asc'?'desc':'asc';saveTable();render();};
    const groupLabel=el('label',undefined,'workspace-check'),check=el('input');check.type='checkbox';check.checked=groupRepos;check.dataset.githubControl='group';check.onchange=()=>{groupRepos=check.checked;saveTable();render();};groupLabel.append(check,document.createTextNode('Group by repository'));
    bar.append(label,direction,groupLabel);area.append(bar);
    const panel=el('details',undefined,'github-column-filters');panel.open=filtersOpen;const count=Object.values(filters).filter(Boolean).length;panel.append(el('summary',`Column filters${count?` · ${count} active`:''}`));panel.ontoggle=()=>{if(panel.isConnected)filtersOpen=panel.open;};
    const grid=el('div',undefined,'github-filter-grid');
    const fields=[['title','Pull request'],['repository','Repository'],['author','Author'],['included','Included because',[['authored','Created by me'],['assigned','Assigned to me'],['reviewRequested','Review requested']]],['review','Review',['Approved','In review','Changes requested','No review','Draft','Unknown']],['ci','CI',['Passing','Failing','Pending','Waiting','Unknown']],['updatedFrom','Updated from'],['updatedThrough','Updated through'],['createdFrom','Created from'],['createdThrough','Created through'],['draftReview','Draft review',[['pending','Unsubmitted review'],['none','No unsubmitted review'],['unknown','Unavailable']]]];
    for(const [key,title,options] of fields){
      const label=el('label',title),input=el(options?'select':'input');input.dataset.githubControl=key;input.setAttribute('aria-label',`Filter ${title}`);
      if(options){const all=el('option','Any');all.value='';input.append(all);for(const entry of options){const [v,text]=Array.isArray(entry)?entry:[entry,entry],option=el('option',text);option.value=v;input.append(option);}}
      else{input.type=/From$|Through$/.test(key)?'date':'search';if(input.type==='search')input.placeholder=key==='title'?'Title, PR number or branch':'Contains…';}
      input.value=filters[key];input.oninput=()=>{filters[key]=input.value;render();};label.append(input);grid.append(label);
    }
    const clear=el('button','Clear filters','text-button');clear.onclick=()=>{for(const key in filters)filters[key]='';render();};grid.append(clear);panel.append(grid);area.append(panel);return area;
  }
  function dateCell(item,field){const cell=el('td'),raw=item.pr[field==='updated'?'updatedAt':'createdAt'];if(timestamp(raw)===null){cell.textContent='Unknown';return cell;}const time=el('time',new Date(raw).toLocaleString(undefined,{year:'numeric',month:'short',day:'numeric',hour:'2-digit',minute:'2-digit'}));time.dateTime=raw;time.title=raw;cell.append(time);return cell;}
  function table(host,items,category){
    const scroll=el('div',undefined,'github-browser-scroll'),table=el('table',undefined,'github-pr-table'),head=el('thead'),heading=el('tr');table.setAttribute('aria-label',`${host.host} · ${category}`);
    for(const [field,title] of columns){const cell=el('th'),button=el('button',`${title}${sort.field===field?(sort.direction==='desc'?' ↓':' ↑'):''}`,'text-button');cell.scope='col';cell.setAttribute('aria-sort',sort.field===field?(sort.direction==='desc'?'descending':'ascending'):'none');button.dataset.githubControl=`heading:${host.host}:${category}:${field}`;button.setAttribute('aria-label',`Sort by ${title}`);button.onclick=()=>setSort(field);cell.append(button);heading.append(cell);}heading.append(el('th',''));head.append(heading);table.append(head);
    const repos=new Map();for(const row of sorted(items)){const key=groupRepos?row.repository.owner+'/'+row.repository.name:'';if(!repos.has(key))repos.set(key,[]);repos.get(key).push(row);}
    for(const [name,items] of repos){
      const key=host.host+'/'+name,body=el('tbody');
      if(groupRepos){const groupRow=el('tr'),cell=el('th'),toggle=el('button',`${collapsed.has(key)?'▸':'▾'} ${name} · ${items.length}`,'text-button');cell.colSpan=columns.length+1;cell.scope='rowgroup';toggle.dataset.githubControl=`repo:${host.host}:${category}:${name}`;toggle.setAttribute('aria-expanded',String(!collapsed.has(key)));toggle.setAttribute('aria-label',`Toggle ${host.host}/${name}`);cell.append(toggle);groupRow.append(cell);body.append(groupRow);
        toggle.onclick=()=>{if(collapsed.has(key))collapsed.delete(key);else collapsed.add(key);try{localStorage.setItem('agentworks-github-collapsed',JSON.stringify([...collapsed]));}catch{}render();};}
      for(const item of items){
        const pr=item.pr,prKey=JSON.stringify([host.host,item.repository.owner,item.repository.name,pr.number]),row=el('tr',undefined,'github-browser-row');row.dataset.prKey=prKey;row.dataset.number=pr.number;row.tabIndex=0;row.hidden=groupRepos&&collapsed.has(key);row.classList.toggle('has-pending-review',draftState(item)==='pending');rows.set(prKey,item);
        const title=el('td'),link=el('a',item.badge.label,`session-badge ${item.badge.tone}`),href=destination(pr.url);link.title=item.badge.tooltip;if(href){link.href=href;link.target='_blank';link.rel='noopener noreferrer';}const alert=draftAlert(item);if(alert)title.append(alert);title.append(link,el('span',pr.title,'github-pr-title'),el('small',pr.headBranch||'','field-note'));
        row.append(title,el('td',`${item.repository.owner}/${item.repository.name}`),el('td',pr.author||'Unknown'),el('td',included(item)),el('td',review(item)),el('td',ci(item)),dateCell(item,'updated'),dateCell(item,'created'));
        const action=el('td'),open=el('button','Open in session…','text-button');open.onclick=()=>pivot(item);action.append(open);row.append(action);body.append(row);
      }
      table.append(body);
    }
    scroll.append(table);return scroll;
  }
  function render(){
    // Native details toggles dispatch their event later; read the live state
    // before replacing controls in response to the next input event.
    const filterPanel=root.querySelector('.github-column-filters');if(filterPanel)filtersOpen=filterPanel.open;
    const focus=document.activeElement,control=focus?.dataset.githubControl,selection=typeof focus?.selectionStart==='number'?[focus.selectionStart,focus.selectionEnd]:null;
    const viewport=root.closest('#workspace-github'),scrollTop=viewport?.scrollTop;
    const tableScroll=new Map([...root.querySelectorAll('.github-browser-scroll')].map(n=>[n.querySelector('table')?.getAttribute('aria-label'),n.scrollLeft]));
    root.replaceChildren();rows.clear();const header=el('div',undefined,'github-browser-heading');header.append(el('h1','GitHub'),el('span','Open PRs created by you, assigned to you, or awaiting your review','field-note'));const settings=el('button','Connection and polling settings','text-button');settings.onclick=openSettings;header.append(settings);root.append(header);
    if(!data){root.append(el('p','Loading pull requests…','field-note'));return;}
    if(!data.hosts.length){root.append(el('p','Enable a GitHub account in Settings to see your pull requests.','field-note'));return;}
    const reminder=draftSummary();if(reminder)root.append(reminder);
    const unknown=data.hosts.flatMap(h=>h.rows).filter(item=>draftState(item)==='unknown').length;
    if(unknown)root.append(el('p',`Draft review status is unavailable for ${unknown} ${unknown===1?'PR':'PRs'}.`,'field-note'));
    root.append(controls());
    for(const host of data.hosts){
      const filtered=host.rows.filter(matches),section=group('host:'+host.host,`${host.host} · ${filtered.length}${filtered.length!==host.rows.length?` of ${host.rows.length}`:''} open ${host.rows.length===1?'PR':'PRs'}`);section.append(el('p',`${host.accounts.join(', ')} · Poll every ${host.intervalMs/60000} min`,'field-note'));
      for(const message of host.messages)section.append(el('p',`${message.login}: ${message.message}`,'field-note'));
      for(const [kind,title] of [['requested','Review requested'],['other','Created or assigned']]){
        const items=filtered.filter(item=>kind==='requested'?item.reviewRequested:!item.reviewRequested);
        if(!items.length)continue;
        const category=el('section',undefined,'github-pr-category');category.dataset.category=kind;category.append(el('h2',`${title} · ${items.length}`),table(host,items,title));section.append(category);
      }
      if(!filtered.length)section.append(el('p',host.rows.length?'No PRs match the column filters.':host.messages.length?'Pull requests are unavailable; check the connection messages above.':'No open PRs found for these accounts.','field-note'));root.append(section);
    }
    if(control){const replacement=[...root.querySelectorAll('[data-github-control]')].find(n=>n.dataset.githubControl===control);replacement?.focus({preventScroll:true});if(selection&&replacement?.type==='search')replacement.setSelectionRange(...selection);}
    for(const n of root.querySelectorAll('.github-browser-scroll'))n.scrollLeft=tableScroll.get(n.querySelector('table')?.getAttribute('aria-label'))||0;
    if(viewport&&scrollTop!==undefined)viewport.scrollTop=scrollTop;
  }
  function updateNavigation(unavailable=false){
    const nav=document.getElementById('nav-github'),mark=document.getElementById('github-nav-warning');
    const pending=(data?.hosts||[]).flatMap(host=>host.rows).filter(item=>draftState(item)==='pending');
    const stale=unavailable||pending.some(item=>item.stale||item.pendingReviews?.stale);
    const message=pending.length?`${pending.length} ${pending.length===1?'PR has':'PRs have'} unsubmitted reviews${stale?' · last seen; current status unavailable':''}`:'';
    if(mark)mark.hidden=!pending.length;
    if(nav){nav.title=message||'GitHub';nav.setAttribute('aria-label',message?`GitHub · ${message}`:'GitHub');}
  }
  async function load(){
    if(document.hidden||busy)return;busy=true;clearTimeout(timer);let failed=false;
    try{data=await api('/api/github/pulls');updateNavigation();if(active)render();}
    catch(error){failed=true;updateNavigation(true);if(active)root.replaceChildren(el('h1','GitHub'),el('p',error.message,'error-text'));}
    finally{busy=false;if(!document.hidden){const due=failed?Date.now()+Math.min(...(data?.hosts||[]).map(h=>h.intervalMs||300000),300000):Math.min(...(data?.hosts||[]).map(h=>h.nextPollAt||Date.now()+300000),Date.now()+300000);timer=setTimeout(load,Math.max(1000,due-Date.now()));}}
  }
  document.addEventListener('visibilitychange',()=>{clearTimeout(timer);if(!document.hidden)load();});
  document.addEventListener('github-polling-changed',()=>{clearTimeout(timer);load();});
  load();
  return {
    setActive(value){if(active===value)return;active=value;if(value)render();else if(chooser.open)closePivot();},
    context(target){const row=target.closest('.github-browser-row'),item=row&&rows.get(row.dataset.prKey);if(!item)return null;return {origin:row,focus:row.querySelector('button'),label:`Actions for PR #${item.pr.number}`,actions:[{label:'Open in session…',run:()=>pivot(item)},{label:'New session…',run:()=>openSession(item)},...(destination(item.pr.url)?[{label:'Open PR in new tab',run:()=>window.open(destination(item.pr.url),'_blank','noopener,noreferrer')}]:[])]};},
  };
}
