import {localState as localStorage} from './hosting.js';
// Broad PR membership is opt-in: this view issues requests only while visible.
export function setupGithubBrowser({api,root,openSession,openSettings}){
  const el=(tag,text,cls)=>{const n=document.createElement(tag);if(text!==undefined)n.textContent=text;if(cls)n.className=cls;return n;};
  let active=false,busy=false,generation=0,timer=null,data=null;
  const rows=new Map(),collapsed=new Set();
  try{for(const key of JSON.parse(localStorage.getItem('agentworks-github-collapsed')||'[]'))collapsed.add(key);}catch{}
  function group(key,title){const details=el('details',undefined,'github-browser-group');details.open=!collapsed.has(key);details.append(el('summary',title));details.ontoggle=()=>{if(details.open)collapsed.delete(key);else collapsed.add(key);try{localStorage.setItem('agentworks-github-collapsed',JSON.stringify([...collapsed]));}catch{}};return details;}
  function destination(value){try{const u=new URL(value);return u.protocol==='https:'&&!u.username&&!u.password?u.href:null;}catch{return null;}}
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
          const action=el('td'),open=el('button','Open in session…','text-button');open.onclick=()=>openSession(item);action.append(open);row.append(action);body.append(row);
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
    setActive(value){if(active===value)return;active=value;generation++;clearTimeout(timer);if(value){render();load();}},
    context(target){const row=target.closest('.github-browser-row'),item=row&&rows.get(row.dataset.prKey);if(!item)return null;return {origin:row,focus:row.querySelector('button'),label:`Actions for PR #${item.pr.number}`,actions:[{label:'Open in session…',run:()=>openSession(item)}]};},
  };
}
