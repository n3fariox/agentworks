// Usage: node scripts/timeout-smoke.mjs <isolated base URL> <root dir> (temp catalog/db/demo state; ~60s).
// L-07 runtime fixture: demo agent, no paid inference. Total 60s limit fires during a
// pending approval after quota evidence; turn is cancelled, marked, never resumed.
const base=process.argv[2],cwd=process.argv[3],headers={Origin:base,'Content-Type':'application/json'};
const api=async(p,m='GET',b)=>{const r=await fetch(base+p,{method:m,headers,body:b&&JSON.stringify(b)});const t=await r.text();if(!r.ok)throw new Error(p+' '+r.status+' '+t);return JSON.parse(t);};
const s=await api('/api/sessions','POST',{provider:'demo',cwd,title:'timeout fixture',resumePolicy:{enabled:true}});
await api(`/api/sessions/${s.id}/timeouts`,'PUT',{policy:{totalSeconds:60,inactivityWarningSeconds:null,inactivityTimeoutSeconds:null,cancelGraceSeconds:5}});
for(let i=0;i<50;i++){const g=await api(`/api/sessions/${s.id}`);if((g.session?.status??g.status)==='idle')break;await new Promise(r=>setTimeout(r,200));}
await api(`/api/sessions/${s.id}/prompt`,'POST',{text:'quota active-approval',requestId:crypto.randomUUID()});
const t0=Date.now();let snap;
for(;;){snap=await api(`/api/sessions/${s.id}`);const kinds=snap.events.map(e=>e.kind);if(kinds.includes('turn_end')||kinds.includes('error')||Date.now()-t0>90000)break;await new Promise(r=>setTimeout(r,500));}
const ev=k=>snap.events.filter(e=>e.kind===k).map(e=>e.data);
const out={elapsedS:Math.round((Date.now()-t0)/1000),policy:ev('timeout_policy')[0]?.policy?.totalSeconds,timeout:ev('timeout'),turnEnd:ev('turn_end'),errors:ev('error'),continuation:snap.session?.continuation};
console.log(JSON.stringify(out));
const ok=out.policy===60&&out.timeout[0]?.kind==='total'&&out.elapsedS>=58&&out.turnEnd[0]?.timedOut==='total'&&!out.continuation?.dueAt;
console.log(ok?'PASS':'FAIL');process.exit(ok?0:1);
