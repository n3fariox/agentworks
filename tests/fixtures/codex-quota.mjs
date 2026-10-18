#!/usr/bin/env node
// Read-only app-server protocol fixture: reject every unanticipated RPC.
import readline from 'node:readline';
const send=v=>process.stdout.write(JSON.stringify(v)+'\n');
readline.createInterface({input:process.stdin}).on('line',line=>{
 const m=JSON.parse(line);
 if(m.method==='initialize')send({id:m.id,result:{}});
 else if(m.method==='initialized'){}
 else if(m.method==='account/rateLimits/read')send({id:m.id,result:{rateLimitsByLimitId:{codex:{primary:{usedPercent:100,resetsAt:Math.floor(Date.now()/1000)+100},secondary:{usedPercent:100,resetsAt:Math.floor(Date.now()/1000)+500}},other:{primary:{usedPercent:40,resetsAt:9999999999}}}}});
 else {send({id:m.id,error:{code:-32601,message:'No thread/turn operations permitted'}});process.exitCode=1;}
});
