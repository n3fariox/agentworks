import {localState as localStorage} from './hosting.js';
// Broad PR membership is opt-in: this view issues requests only while visible.
export function setupGithubBrowser({api,root,openSession,selectSession,openSettings}){
  const el=(tag,text,cls)=>{const n=document.createElement(tag);if(text!==undefined)n.textContent=text;if(cls)n.className=cls;return n;};
  let active=false,busy=false,generation=0,timer=null,data=null;
  const rows=new Map(),collapsed=new Set();
  try{for(const key of JSON.parse(localStorage.getItem('agentworks-github-collapsed')||'[]'))collapsed.add(key);}catch{}
  function group(key,title){const details=el('details',undefined,'github-browser-group');details.open=!collapsed.has(key);details.append(el('summary',title));details.ontoggle=()=>{if(details.open)collapsed.delete(key);else collapsed.add(key);try{localStorage.setItem('agentworks-github-collapsed',JSON.stringify([...collapsed]));}catch{}};return details;}
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
  function render(){
    root.replaceChildren();rows.clear();const header=el('div',undefined,'github-browser-heading');header.append(el('h1','GitHub'),el('span','Open PRs created by you, assigned to you, or awaiting your review','field-note'));const settings=el('button','Connection and polling settings','text-button');settings.onclick=openSettings;header.append(settings);root.append(header);
    if(!data){root.append(el('p','Loading pull requests…','field-note'));return;}
    if(!data.hosts.length){root.append(el('p','Enable a GitHub account in Settings to see your pull requests.','field-note'));return;}
    for(const host of data.hosts){
      const section=group('host:'+host.host,`${host.host} · ${host.rows.length} open ${host.rows.length===1?'PR':'PRs'}`);section.append(el('p',`${host.accounts.join(', ')} · Poll every ${host.intervalMs/60000} min`,'field-note'));
      for(const message of host.messages)section.append(el('p',`${message.login}: ${message.message}`,'field-note'));
      const repos=new Map();for(const row of host.rows){const key=row.repository.owner+'/'+row.repository.name;if(!repos.has(key))repos.set(key,[]);repos.get(key).push(row);}
      const scroll=el('div',undefined,'github-browser-scroll'),table=el('table',undefined,'github-pr-table'),head=el('thead'),heading=el('tr');
      for(const title of ['Pull request','Author','Included because','Review','CI',''])heading.append(el('th',title));head.append(heading);table.append(head);
      for(const [name,items] of repos){
        const key=host.host+'/'+name,body=el('tbody'),groupRow=el('tr'),cell=el('th'),toggle=el('button',`${collapsed.has(key)?'▸':'▾'} ${name} · ${items.length}`,'text-button');cell.colSpan=6;cell.scope='rowgroup';toggle.setAttribute('aria-expanded',String(!collapsed.has(key)));toggle.setAttribute('aria-label',`Toggle ${host.host}/${name}`);cell.append(toggle);groupRow.append(cell);body.append(groupRow);
        for(const item of items){
          const pr=item.pr,prKey=JSON.stringify([host.host,name,pr.number]),row=el('tr',undefined,'github-browser-row');row.dataset.prKey=prKey;row.dataset.number=pr.number;row.tabIndex=0;row.hidden=collapsed.has(key);rows.set(prKey,item);
          const title=el('td'),link=el('a',item.badge.label,`session-badge ${item.badge.tone}`),href=destination(pr.url);link.title=item.badge.tooltip;if(href){link.href=href;link.target='_blank';link.rel='noopener noreferrer';}title.append(link,el('span',pr.title,'github-pr-title'),el('small',pr.headBranch||'','field-note'));row.append(title,el('td',pr.author||'Unknown'),el('td',[item.authored?'Created by me':'',item.assigned?'Assigned to me':'',item.reviewRequested?'Review requested':''].filter(Boolean).join(' · ')),el('td',pr.draft?'Draft':({APPROVED:'Approved',REVIEW_REQUIRED:'In review',CHANGES_REQUESTED:'Changes requested',NONE:'No review',UNKNOWN:'Unknown'}[pr.reviewSummary?.state||pr.reviewDecision]||'Unknown')),el('td',({SUCCESS:'Passing',FAILURE:'Failing',ERROR:'Failing',PENDING:'Pending',EXPECTED:'Waiting'}[pr.checks?.state]||'Unknown')));
          const action=el('td'),open=el('button','Open in session…','text-button');open.onclick=()=>pivot(item);action.append(open);row.append(action);body.append(row);
        }
        toggle.onclick=()=>{const open=toggle.getAttribute('aria-expanded')!=='true';toggle.setAttribute('aria-expanded',String(open));toggle.textContent=`${open?'▾':'▸'} ${name} · ${items.length}`;for(const row of body.querySelectorAll('.github-browser-row'))row.hidden=!open;if(open)collapsed.delete(key);else collapsed.add(key);try{localStorage.setItem('agentworks-github-collapsed',JSON.stringify([...collapsed]));}catch{}};
        table.append(body);
      }
      scroll.append(table);section.append(scroll);
      if(!host.rows.length)section.append(el('p',host.messages.length?'Pull requests are unavailable; check the connection messages above.':'No open PRs found for these accounts.','field-note'));root.append(section);
    }
  }
  async function load(){
    if(!active||document.hidden||busy)return;const version=generation;busy=true;
    try{const next=await api('/api/github/pulls');if(version!==generation)return;data=next;render();}
    catch(error){if(version===generation){root.replaceChildren(el('h1','GitHub'),el('p',error.message,'error-text'));}}
    finally{busy=false;if(active&&!document.hidden){if(version!==generation){load();return;}const due=Math.min(...(data?.hosts||[]).map(h=>h.nextPollAt||Date.now()+300000),Date.now()+300000);timer=setTimeout(load,Math.max(1000,due-Date.now()));}}
  }
  document.addEventListener('visibilitychange',()=>{clearTimeout(timer);if(active&&!document.hidden)load();});
  document.addEventListener('github-polling-changed',()=>{clearTimeout(timer);if(active)load();});
  return {
    setActive(value){if(active===value)return;active=value;generation++;clearTimeout(timer);if(value){render();load();}else if(chooser.open)closePivot();},
    context(target){const row=target.closest('.github-browser-row'),item=row&&rows.get(row.dataset.prKey);if(!item)return null;return {origin:row,focus:row.querySelector('button'),label:`Actions for PR #${item.pr.number}`,actions:[{label:'Open in session…',run:()=>pivot(item)},{label:'New session…',run:()=>openSession(item)}]};},
  };
}
