// Regression: modern Claude MCP discovery requires resultType and private cache hints.
import net from 'node:net';
import {spawn} from 'node:child_process';
import readline from 'node:readline';
import {resolve} from 'node:path';
import assert from 'node:assert/strict';
const bridge=net.createServer(socket=>{
 let bytes='';socket.on('data',chunk=>{bytes+=chunk;if(bytes.includes('\n')){const request=JSON.parse(bytes);assert.equal(request.token,'wire-test-only');const value=request.operation==='tools/list'?{tools:[{name:'read_inbox',description:'Read inbox',inputSchema:{type:'object',properties:{},additionalProperties:false}}]}:{ok:true};socket.end(JSON.stringify(value)+'\n');}});
});
await new Promise(r=>bridge.listen(0,'127.0.0.1',r));
try{
 for(const modern of [false,true]){
  const p=spawn(resolve('target/debug/agentworks'),['mcp'],{env:{...process.env,AGENTWORKS_MCP_ENDPOINT:`127.0.0.1:${bridge.address().port}`,AGENTWORKS_MCP_TOKEN:'wire-test-only'},stdio:['pipe','pipe','inherit']});
  const lines=readline.createInterface({input:p.stdout}),pending=new Map();let sequence=0;
  lines.on('line',line=>{const v=JSON.parse(line);pending.get(v.id)?.(v);pending.delete(v.id);});
  const meta={'io.modelcontextprotocol/protocolVersion':'2026-07-28','io.modelcontextprotocol/clientInfo':{name:'wire-test',version:'1'},'io.modelcontextprotocol/clientCapabilities':{}};
  async function request(method,params){const id=++sequence;const result=new Promise((r,reject)=>{const timer=setTimeout(()=>reject(Error('MCP response timed out')),5000);pending.set(id,v=>{clearTimeout(timer);r(v);});});p.stdin.write(JSON.stringify({jsonrpc:'2.0',id,method,params:modern?{...params,_meta:meta}:params})+'\n');return result;}
  try{
   if(modern){const r=await request('server/discover',{});assert.equal(r.result.resultType,'complete');}
   else {const r=await request('initialize',{protocolVersion:'2025-11-25',capabilities:{},clientInfo:{name:'wire-test',version:'1'}});assert.equal(r.result.serverInfo.name,'agentworks');p.stdin.write(JSON.stringify({jsonrpc:'2.0',method:'notifications/initialized'})+'\n');}
   const list=await request('tools/list',{});assert.equal(list.result.tools[0].name,'read_inbox');
   if(modern){assert.equal(list.result.resultType,'complete');assert.equal(list.result.ttlMs,0);assert.equal(list.result.cacheScope,'private');}
   const call=await request('tools/call',{name:'read_inbox',arguments:{}});assert.equal(JSON.parse(call.result.content[0].text).ok,true);if(modern)assert.equal(call.result.resultType,'complete');
  }finally{p.kill('SIGTERM');lines.close();}
 }
 console.log('PASS: native MCP stdio legacy and modern discovery, tools/list cache isolation, and tool results');
}finally{bridge.close();}
