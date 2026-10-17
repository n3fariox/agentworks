// Opt-in: import populated native test sessions from a previous test:persistence run. No new inference.
import assert from 'node:assert/strict';
import {DatabaseSync} from 'node:sqlite';
const fixturePath=process.env.AGENTWORKS_IMPORT_FIXTURE_DB;
assert(fixturePath,'Set AGENTWORKS_IMPORT_FIXTURE_DB to a native persistence-test database');
const db=new DatabaseSync(fixturePath,{readOnly:true});
const fixtures=db.prepare('SELECT metadata,state FROM sessions').all().map(row=>({meta:JSON.parse(row.metadata),state:JSON.parse(row.state)})).filter(s=>['claude','codex','opencode'].includes(s.meta.provider)&&s.meta.title.startsWith('Native resume '));
assert.equal(fixtures.length,3,'Expected one populated native test conversation per harness');
const base=process.env.AGENTWORKS_URL||'http://127.0.0.1:4320',headers={Origin:base,'Content-Type':'application/json'};
async function api(path,method='GET',body){const r=await fetch(base+path,{method,headers,body:body===undefined?undefined:JSON.stringify(body)});assert.equal(r.status,200,await r.clone().text());return r.json();}
for(const {meta,state}of fixtures){
  const providerId=state.provider_session_id;let found=false,cursor=null;
  for(let n=0;n<20;n++){
    const page=await api('/api/external-sessions','POST',{provider:meta.provider,cwd:meta.cwd,cursor,hostProfile:false});
    if(page.sessions.some(s=>s.sessionId===providerId)){found=true;break;}
    cursor=page.nextCursor;if(!cursor)break;
  }
  assert(found,meta.provider+' did not list its saved test conversation');
  const s=await api('/api/sessions/import','POST',{provider:meta.provider,providerSessionId:providerId,cwd:meta.cwd,title:'Native import test '+meta.provider,hostProfile:false});
  try{
    let ready;
    for(let n=0;n<900;n++){
      const snap=await api('/api/sessions/'+s.id);
      if(snap.session.status==='error')throw new Error(snap.events.filter(e=>e.kind==='error').map(e=>e.data.message).join('\n'));
      if(snap.session.status==='idle'){ready=snap;break;}
      await new Promise(r=>setTimeout(r,100));
    }
    assert(ready,'Import timed out');
    const replay=ready.events.filter(e=>e.kind==='acp'),users=replay.filter(e=>e.data.sessionUpdate==='user_message_chunk');
    assert(users.length>=2,meta.provider+' did not replay prior user turns');
    const text=replay.filter(e=>e.data.sessionUpdate==='agent_message_chunk').map(e=>e.data.content?.text||'').join('');
    assert(text.includes('MEMORY_'),meta.provider+' did not import the historical model reply');
    const originalSeq=ready.seq;await api('/api/sessions/'+s.id+'/resume','POST',{});
    const again=await api('/api/sessions/'+s.id);assert.equal(again.seq,originalSeq);
    console.log(`PASS: ${meta.provider} listed and imported its original conversation, user turns and model replies without inference`);
  }finally{await api('/api/sessions/'+s.id,'DELETE');}
}
db.close();
