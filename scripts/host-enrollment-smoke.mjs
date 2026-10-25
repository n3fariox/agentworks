// Real SSH and PTY integration, isolated HOME/authorized_keys/known_hosts.
// The remote has no Node, npm or mise: this check makes no downloads/sign-ins.
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {once} from 'node:events';
import {mkdtemp,mkdir,writeFile,readFile,readdir,stat,rm,symlink} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {tmpdir,userInfo} from 'node:os';
import {createServer} from 'node:net';
import WebSocket from 'ws';
const q=s=>`'${s.replaceAll("'","'\\''")}'`;
const dir=await mkdtemp(join(tmpdir(),'agentworks-enroll-'));
const home=join(dir,"remote home 'quoted'"),bin=join(dir,'bin');
await mkdir(join(home,'.ssh'),{recursive:true});await mkdir(bin);
const realMise=process.env.AGENTWORKS_ENROLL_REAL_MISE==='1';
const secret='enrollment-test-passphrase';
const seed=join(dir,'seed'),hostkey=join(dir,'hostkey');
for(const [file,pass]of [[seed,secret],[hostkey,'']])execFileSync('ssh-keygen',['-q','-t','ed25519','-N',pass,'-f',file]);
await writeFile(join(home,'.ssh/authorized_keys'),await readFile(seed+'.pub'),{mode:0o600});
// The remote home has neither mise nor Node: the worker must install and run without them.
if(realMise)await symlink(process.env.AGENTWORKS_TEST_MISE||join(userInfo().homedir,'.local/bin/mise'),join(bin,'mise'));
await writeFile(join(dir,'remote-shell'),`#!/bin/sh
export HOME=${q(home)}
export PATH=${q(bin+':/usr/bin:/bin')}
export AGENTWORKS_DEMO_STATE_DIR=${q(join(dir,'demo'))}
exec /bin/sh -c "$SSH_ORIGINAL_COMMAND"
`,{mode:0o700});
const freePort=async()=>{const s=createServer();await new Promise(r=>s.listen(0,'127.0.0.1',r));const p=s.address().port;await new Promise(r=>s.close(r));return p;};
const sshPort=await freePort(),port=await freePort(),base=`http://127.0.0.1:${port}`;
await writeFile(join(dir,'sshd_config'),`Port ${sshPort}
ListenAddress 127.0.0.1
HostKey ${hostkey}
PidFile ${dir}/sshd.pid
AuthorizedKeysFile "${home}/.ssh/authorized_keys"
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
UsePAM no
StrictModes no
ForceCommand ${dir}/remote-shell
LogLevel ERROR
`);
// Supply a passphrase-protected existing key only for the interactive bootstrap.
await writeFile(join(dir,'ssh'),`#!/bin/bash
set -eu
managed=false
for arg in "$@"; do if [ "$arg" = -i ]; then managed=true; fi; done
if "$managed"; then exec /usr/bin/ssh "$@"; fi
exec /usr/bin/ssh -i ${q(seed)} -o IdentitiesOnly=yes -o IdentityAgent=none "$@"
`,{mode:0o700});
let server,sshd,log='';const sockets=new Set();
const delay=ms=>new Promise(r=>setTimeout(r,ms));
const env={...process.env};for(const key of Object.keys(env))if(key.startsWith('AGENTWORKS_'))delete env[key];
const catalog=join(dir,'catalog with spaces.json');
async function start(){
 server=spawn(resolve('target/debug/agentworks'),[],{env:{...env,AGENTWORKS_PORT:String(port),AGENTWORKS_ROOTS:resolve('.'),AGENTWORKS_CATALOG_PATH:catalog,AGENTWORKS_SSH:join(dir,'ssh')},stdio:['ignore','pipe','pipe']});
 server.stdout.on('data',b=>log+=b);server.stderr.on('data',b=>log+=b);
 for(let n=0;n<100;n++){try{if((await fetch(base+'/api/status')).ok)return;}catch{}await delay(50);}throw Error('Server did not start: '+log);
}
async function stop(child){if(!child||child.exitCode!==null)return;const done=once(child,'exit');child.kill('SIGTERM');await done;}
const api=async(path,body,method='POST')=>{const r=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});assert.equal(r.status,200,await r.clone().text());return r.json();};
async function enroll(name,destination,{cancel=false}={}){
 const ws=new WebSocket(base.replace('http','ws')+'/api/hosts/enroll/events',{headers:{Origin:base}});sockets.add(ws);
 let text='',sentYes=false,sentPass=false;
 const result=new Promise((ok,fail)=>{
  const timer=setTimeout(()=>fail(Error('Enrollment timed out: '+text+'\n'+log)),180000);
  ws.on('open',()=>ws.send(JSON.stringify({name,destination})));
  ws.on('error',fail);
  ws.on('message',(buf,binary)=>{
   if(binary){
    text+=buf.toString();
    if(!sentYes&&text.includes('Are you sure you want to continue connecting')){sentYes=true;ws.send(JSON.stringify({type:'input',data:'yes\r'}));}
    if(!sentPass&&text.includes('Enter passphrase')){sentPass=true;if(cancel){clearTimeout(timer);ws.close();ok({cancelled:true,text});return;}ws.send(JSON.stringify({type:'input',data:secret+'\r'}));}
    return;
   }
   const event=JSON.parse(buf);
   if(event.type==='error'||event.type==='complete'){clearTimeout(timer);ws.close();ok({...event,text});}
  });
 });
 const value=await result;assert(!value.text.includes(secret),'Password/passphrase must never echo');return value;
}
try{
 sshd=spawn('/usr/bin/sshd',['-D','-e','-f',join(dir,'sshd_config')],{stdio:['ignore','pipe','pipe']});sshd.stderr.on('data',b=>log+='sshd: '+b);
 await delay(200);assert.equal(sshd.exitCode,null,log);await start();
 // This WebSocket runs commands, so it must pass the same-origin guard.
 const rejected=new WebSocket(base.replace('http','ws')+'/api/hosts/enroll/events',{headers:{Origin:'https://other.example'}});
 const code=await new Promise(r=>{rejected.on('unexpected-response',(_,res)=>{res.resume();r(res.statusCode);});rejected.on('error',()=>{});});assert.equal(code,403);
 assert.equal((await enroll('Invalid','-oProxyCommand=sh')).type,'error');
 const destination=`ssh://${userInfo().username}@127.0.0.1:${sshPort}`;
 const cancelled=await enroll('Cancelled',destination,{cancel:true});assert(cancelled.cancelled);
 await delay(200);assert.equal((await api('/api/catalog',undefined,'GET')).hosts.length,1);
 assert.deepEqual(await readdir(join(dir,'host-keys')),[]);
 const installed=await enroll('Installed',destination);assert.equal(installed.type,'complete',installed.text+'\n'+installed.message);assert.equal(installed.host.status,'connected');assert(installed.host.managed);assert(installed.host.worker.providers.includes('demo'));
 const stored=JSON.parse(await readFile(catalog));const host=stored.hosts[0];
 assert.equal((await stat(host.identityFile)).mode&0o777,0o600);assert.equal((await stat(resolve(host.identityFile,'..'))).mode&0o777,0o700);
 assert(!('identityFile' in installed.host));assert(!('knownHostsFile' in installed.host));
 const keys=await readFile(join(home,'.ssh/authorized_keys'),'utf8');assert.match(keys,/restrict ssh-ed25519 .* agentworks:/);
 for(let n=0;n<100;n++){const files=(await readdir(resolve(host.identityFile,'..'))).sort();if(files.join(',')==='id_ed25519,id_ed25519.pub,known_hosts')break;if(n===99)assert.fail('Temporary bundle not cleaned: '+files);await delay(50);}
 await api(`/api/hosts/${host.id}/roots`,{roots:[home]},'PUT');
 const session=await api('/api/sessions',{provider:'demo',cwd:home,hostId:host.id,title:'Enrolled host'});
 for(let n=0;n<200;n++){const s=await api(`/api/sessions/${session.id}`,undefined,'GET');if(s.session.status==='idle')break;assert.notEqual(s.session.status,'error',JSON.stringify(s.events));if(n===199)throw Error('Remote demo did not start');await delay(50);}
 // Restart without any SSH agent: the dedicated key must be sufficient.
 await stop(server);await start();
 for(let n=0;n<200;n++){const h=(await api('/api/catalog',undefined,'GET')).hosts.find(h=>h.id===host.id);if(h.status==='connected')break;if(n===199)throw Error('Managed host did not reconnect: '+JSON.stringify(h));await delay(50);}
 // A changed host identity must never silently become trusted.
 const pinned=await readFile(host.knownHostsFile,'utf8');
 const oldKey=(await readFile(hostkey+'.pub','utf8')).split(' ')[1];
 const otherKey=(await readFile(seed+'.pub','utf8')).split(' ')[1];
 assert(pinned.includes(oldKey));await writeFile(host.knownHostsFile,pinned.replaceAll(oldKey,otherKey));
 await api(`/api/hosts/${host.id}/reconnect`,{});
 for(let n=0;n<200;n++){const h=(await api('/api/catalog',undefined,'GET')).hosts.find(h=>h.id===host.id);if(h.status==='offline'){assert.match(h.detail,/IDENTIFICATION HAS CHANGED|Host key verification failed/);break;}if(n===199)throw Error('Changed host key was not rejected');await delay(50);}
 await api(`/api/sessions/${session.id}`,undefined,'DELETE');await api(`/api/hosts/${host.id}`,undefined,'DELETE');
 await assert.rejects(stat(host.identityFile),{code:'ENOENT'});
 assert(!(await readFile(catalog,'utf8')).includes(secret));assert(!log.includes(secret));
 console.log('PASS: real SSH enrollment, hidden passphrase input, host-key confirmation, cancellation, isolated install without Node or mise, remote demo, and restart with managed key'+(realMise?' (launched through real mise)':''));
}catch(error){console.error(log);throw error;}
finally{for(const ws of sockets)ws.close();await stop(server);await stop(sshd);await rm(dir,{recursive:true,force:true});}
