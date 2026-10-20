import {rm} from 'node:fs/promises';

// Stop a running child and await pipe closure before deleting its database/files.
// The fallback bounds teardown when a failed test leaves a child hung.
export async function stopChild(child,{signal='SIGTERM',timeout=5000}={}){
  if(!child||child.pid===undefined)return;
  if(child.exitCode!==null||child.signalCode!==null)return;
  await new Promise((resolve,reject)=>{
    const timer=setTimeout(()=>child.kill('SIGKILL'),timeout);
    const done=error=>{clearTimeout(timer);child.off('close',closed);child.off('error',done);error?reject(error):resolve();};
    const closed=()=>done();
    child.once('close',closed);child.once('error',done);
    child.kill(signal);
  });
}

// Smoke fixtures can be retained deliberately for diagnostics or import checks.
export async function removeFixture(directory){
  if(!directory)return;
  if(process.env.AGENTWORKS_KEEP_TEST_FIXTURES==='1'){
    console.log(`Retained test fixture: ${directory}`);return;
  }
  await rm(directory,{recursive:true,force:true});
}

// A busy fixed port must never make a test mutate another server's fixtures.
export async function serverReady(child,base){
  if(!child?.pid)return false;
  let status;
  try{
    const response=await fetch(base+'/api/service/restart',{signal:AbortSignal.timeout(1000)});
    if(!response.ok)return false;
    status=await response.json();
  }catch{return false;}
  if(status.pid!==child.pid)throw Error(`Test port ${base} belongs to PID ${status.pid}, expected owned child ${child.pid}`);
  return true;
}
