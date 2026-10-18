// Explicit, idempotent preparation for an already imported coordinator. Default: preview only.
import {realpath} from 'node:fs/promises';
import {fileURLToPath} from 'node:url';
const args=process.argv.slice(2),sessionId=args.shift();
const value=name=>{const index=args.indexOf(name);return index<0?null:args[index+1];};
const projectId=value('--project'),nativeId=value('--native'),apply=args.includes('--apply');
if(!sessionId||!projectId||!nativeId)throw Error('Usage: node scripts/prepare-self-improvement.mjs SESSION --project PROJECT --native NATIVE [--service-restart] [--apply] [--url http://127.0.0.1:4318]');
const url=new URL(value('--url')||'http://127.0.0.1:4318');
if(!['http:','https:'].includes(url.protocol)||url.username||url.password||url.search||url.hash)throw Error('Use an HTTP(S) deployment URL without credentials, query or fragment');
if(!url.pathname.endsWith('/'))url.pathname+='/';
const root=await realpath(fileURLToPath(new URL('..',import.meta.url)));
async function api(path,method='GET',body){
  const response=await fetch(new URL(path.replace(/^\//,''),url),{method,headers:{Origin:url.origin,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body),signal:AbortSignal.timeout(20000)});
  if(!response.ok)throw Error(`${method} ${path}: ${response.status} ${await response.text()}`);
  return response.json();
}
const path=`api/sessions/${encodeURIComponent(sessionId)}`;
const snapshot=await api(path),session=snapshot.session;
const identity=snapshot.events.filter(e=>e.kind==='provider_session').at(-1)?.data.id;
if(session.provider!=='codex'||identity!==nativeId||await realpath(session.cwd)!==root)throw Error('Selected session does not match the expected imported Codex identity and repository');
if(!['idle','suspended','error'].includes(session.status)||snapshot.approvals.length||session.turnRunning)throw Error('Finish the current turn and approvals before changing coordinator setup');
const catalog=await api('api/catalog');
if(!catalog.projects.some(p=>p.id===projectId))throw Error('Project does not exist');
const access={communicate:true,startSessions:true,promptSessions:true,cancelSessions:true,runAutomations:false,restartService:args.includes('--service-restart')};
const policy={defaultMode:'newWorktree',requireWorktree:true};
const previousAccess=await api(path+'/agent-access'),previousPolicy=await api(`api/projects/${encodeURIComponent(projectId)}/workspace-policy`);
const changes={sessionId,nativeId,projectId,previousProject:session.projectId,previousAccess,access,previousPolicy,policy};
console.log(JSON.stringify({mode:apply?'apply':'preview',...changes},null,2));
if(apply){
  if(session.projectId!==projectId)await api(path+'/project','POST',{projectId});
  if(JSON.stringify(previousPolicy)!==JSON.stringify(policy))await api(`api/projects/${encodeURIComponent(projectId)}/workspace-policy`,'PUT',policy);
  if(JSON.stringify(previousAccess)!==JSON.stringify(access))await api(path+'/agent-access','PUT',access);
  console.log('Coordinator prepared. No turn, timer or deployment started. Resume explicitly if suspended.');
}
