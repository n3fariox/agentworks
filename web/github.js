import {setupSessionBadges} from './session-badges.js';
// All GitHub credentials stay on the main host. This module receives only
// connection identities, Git metadata and normalized resource snapshots.
export function setupGithub({api,getActive}) {
  const $=id=>document.getElementById(id),button=$('session-github'),dialog=$('github-dialog');
  let generation=0,view=null,busy=false,settingsGeneration=0;
  const browser=setupSessionBadges({api});
  const node=(tag,text,className)=>{const n=document.createElement(tag);if(text!==undefined)n.textContent=text;if(className)n.className=className;return n;};
  function link(text,url){
    const a=node('a',text);try{const parsed=new URL(url);if(parsed.protocol==='https:'&&!parsed.username&&!parsed.password){a.href=parsed.href;a.target='_blank';a.rel='noopener noreferrer';}}catch{}return a;
  }
  const endpoint=()=>`/api/sessions/${encodeURIComponent(getActive().id)}/github`;
  function statusText(data){
    if(data.status==='ready'){
      const prs=data.snapshot.resources.filter(r=>r.kind==='pullRequest');
      const current=prs.filter(r=>r.headMatches);
      if(current.length===1){const pr=current[0];return `PR #${pr.number} · ${pr.draft?'Draft':pr.state.toLowerCase()}`;}
      return prs.length?`${prs.length} pull request${prs.length===1?'':'s'}`:'GitHub';
    }
    return 'GitHub';
  }
  async function load(refresh=false){
    const active=getActive();if(!active)return;
    const gen=++generation;busy=true;button.disabled=true;
    if(dialog.open)$('github-status').textContent='Loading GitHub context…';
    try{
      const data=await browser.read(active.id,refresh);
      if(gen!==generation||getActive()?.id!==active.id)return;
      view=data;button.textContent=statusText(data);button.title=data.message||'GitHub repository, pull requests, issues and checks';
      button.dataset.status=data.status;button.hidden=data.status==='no_origin';
      if(dialog.open)render();
    }catch(error){
      if(gen!==generation)return;
      view={status:'unavailable',message:error.message};button.textContent='GitHub';button.hidden=false;if(dialog.open)render();
    }finally{if(gen===generation){busy=false;button.disabled=false;}}
  }
  function render(){
    const content=$('github-resources'),account=$('github-account'),select=$('github-account-select');
    content.replaceChildren();$('github-linked').replaceChildren();select.replaceChildren();account.hidden=true;
    $('github-status').textContent=(view.message||'')+(view.retryAt?` Retry after ${new Date(view.retryAt).toLocaleTimeString()}.`:'');
    $('github-repository').textContent=view.repository?`${view.repository.host}/${view.repository.owner}/${view.repository.name}`:'';
    $('github-identity').textContent=view.connection?`Account: ${view.connection.login} · credentials on main host`:'';
    $('github-link-controls').hidden=!view.repository||!view.connection;
    $('github-freshness').textContent='';
    for(const candidate of view.accounts||[]){const option=node('option',candidate.login);option.value=candidate.login;select.append(option);}
    if(view.connection)select.value=view.connection.login;
    account.hidden=!(view.accounts?.length);
    for(const attached of view.links||[]){
      const row=node('p',`${attached.kind==='issue'?'Issue':'PR'} #${attached.number} `,'field-note');
      const remove=node('button','Detach','text-button');remove.type='button';remove.onclick=()=>saveLinks(view.links.filter(l=>l!==attached));row.append(remove);$('github-linked').append(row);
    }
    if(view.status!=='ready')return;
    const snapshot=view.snapshot;
    $('github-status').textContent=snapshot.resources.length?'':'No pull requests found for this branch. You can attach an issue or PR by number.';
    $('github-freshness').textContent=`Fetched ${new Date(snapshot.fetchedAt).toLocaleString()} · ${view.branch||'Detached HEAD'} · ${view.commit?.slice(0,10)||'No commit'}${snapshot.complete?'':' · Results are incomplete'}`;
    for(const resource of snapshot.resources){
      const card=node('article',undefined,'github-resource');
      card.append(link(`${resource.kind==='issue'?'Issue':'PR'} #${resource.number}: ${resource.title}`,resource.url));
      const state=[resource.draft?'Draft':resource.state,resource.explicit?'Attached to this session':'From this branch'];
      if(resource.kind==='pullRequest')state.push(resource.headMatches?'Matches checkout commit':'PR head differs from checkout');
      card.append(node('p',state.filter(Boolean).join(' · '),'field-note'));
      if(resource.kind==='pullRequest'){
        const reviewState=resource.reviewSummary?.state||resource.reviewDecision||'UNKNOWN',reviewLabel={APPROVED:'Approved',CHANGES_REQUESTED:'Changes requested',REVIEW_REQUIRED:'Awaiting review',NONE:'Not requested',UNKNOWN:'Unknown'}[reviewState]||'Unknown';
        card.append(node('p',`PR head ${resource.headSha?.slice(0,10)||'unknown'} → ${resource.baseBranch||'unknown'} · Reviews: ${reviewLabel}`,'field-note'));
        const checks=resource.checks;
        card.append(node('p',`Checks for PR head: ${checks.state||'unknown'}${checks.complete?'':' · Details incomplete or unavailable'}`,'field-note'));
        const list=node('ul');
        for(const check of checks.items||[]){const item=node('li');item.append(link(check.name||check.context,check.detailsUrl||check.targetUrl));item.append(document.createTextNode(` · ${check.conclusion||check.status||check.state||'unknown'}`));list.append(item);}
        card.append(list);
      }
      content.append(card);
    }
    if(snapshot.resources.filter(r=>r.kind==='pullRequest'&&!r.explicit).length>1){content.prepend(node('p','Several PRs match this branch. Attach the intended PR explicitly.','field-note'));}
  }
  async function saveLinks(links){
    const active=getActive();if(!active)return;const id=active.id;
    try{await api(endpoint(),{method:'POST',body:JSON.stringify({links})});browser.invalidate();if(getActive()?.id===id)await load();}
    catch(error){if(getActive()?.id===id)$('github-status').textContent=error.message;}
  }
  button.onclick=()=>{dialog.showModal();if(view)render();else load();};
  $('dismiss-github').onclick=()=>dialog.close();
  $('github-refresh').onclick=()=>load(true);
  $('github-account-save').onclick=async()=>{
    const active=getActive();if(!active)return;const id=active.id;
    try{await api(endpoint(),{method:'POST',body:JSON.stringify({login:$('github-account-select').value})});browser.invalidate();if(getActive()?.id===id)await load();}
    catch(error){if(getActive()?.id===id)$('github-status').textContent=error.message;}
  };
  $('github-link-add').onclick=()=>{
    const number=Number($('github-link-number').value),kind=$('github-link-kind').value;
    if(!Number.isSafeInteger(number)||number<1){$('github-status').textContent='Enter an issue or PR number.';return;}
    const repo=view.repository,links=(view.links||[]).filter(l=>l.number!==number||l.kind!==kind);
    links.push({host:repo.host,owner:repo.owner,repo:repo.name,kind,number});saveLinks(links);
  };
  $('github-settings').onclick=()=>{dialog.close();$('nav-settings').click();$('settings-tab-github').click();};
  async function loadSettings(refresh=false){
    const gen=++settingsGeneration,rows=$('github-connections');rows.replaceChildren();$('github-connections-status').textContent='Discovering main-host GitHub accounts…';
    try{
      const data=await api(`/api/github/connections${refresh?'?refresh=true':''}`);if(gen!==settingsGeneration)return;
      $('github-connections-status').textContent=data.message||(data.connections.length?'Connections save immediately.':'No signed-in accounts found. Run gh auth login on the main host, then rediscover.');
      for(const connection of data.connections){
        const row=node('div',undefined,'github-connection'),label=node('label',undefined,'workspace-check');
        const input=node('input');input.type='checkbox';input.checked=connection.enabled;
        label.append(input,document.createTextNode(`${connection.host} · ${connection.login}`));
        const state=node('p',`${connection.state} · ${connection.apiBase}`,'field-note');
        input.onchange=async()=>{input.disabled=true;try{await api('/api/github/connections',{method:'POST',body:JSON.stringify({host:connection.host,login:connection.login,enabled:input.checked})});state.textContent=input.checked?'Enabled':'Disabled';browser.invalidate();sessionChanged();}catch(error){input.checked=!input.checked;state.textContent=error.message;}finally{input.disabled=false;}};
        const verify=node('button','Verify identity','text-button');verify.type='button';verify.onclick=async()=>{verify.disabled=true;try{const result=await api('/api/github/verify',{method:'POST',body:JSON.stringify({host:connection.host,login:connection.login,enabled:true})});state.textContent=result.ok?`Verified ${result.login} on ${result.host}`:result.message;}catch(error){state.textContent=error.message;}finally{verify.disabled=false;}};
        row.append(label,state,verify);rows.append(row);
      }
    }catch(error){if(gen===settingsGeneration)$('github-connections-status').textContent=error.message;}
  }
  $('github-discover').onclick=()=>loadSettings(true);
  $('settings-tab-github').addEventListener('click',()=>loadSettings());
  function sessionChanged(){generation++;view=null;busy=false;button.disabled=false;button.hidden=!getActive();button.textContent='GitHub';if(dialog.open)dialog.close();if(getActive())load();}
  setInterval(()=>{if(getActive()&&!document.hidden&&!busy)load();},60000);
  return {sessionChanged,refreshBadges:browser.sync,updateBadges:browser.update};
}
