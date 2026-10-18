#!/usr/bin/env python3
"""One-shot, bounded Agentworks restart supervisor. Run under systemd; never daemonize."""
import argparse, ctypes, fcntl, json, os, pathlib, signal, subprocess, sys, time, urllib.request, uuid

def atomic(path, value):
    temp=path.with_suffix('.tmp')
    with open(temp,'w') as f:
        json.dump(value,f); f.flush(); os.fsync(f.fileno())
    os.replace(temp,path)

def release(root, name):
    candidate=(root/name).resolve(strict=True)
    if candidate.parent!=root or not candidate.is_file() or not os.access(candidate,os.X_OK) or candidate.stat().st_mode & 0o022:
        raise ValueError('Release must be a regular executable in trusted directory, not group/world writable')
    return candidate

def child_setup():
    # No orphan server if the supervisor is killed. systemd also owns the entire cgroup.
    ctypes.CDLL(None).prctl(1,signal.SIGKILL)
    if os.getppid()!=supervisor_pid: os._exit(1)

parser=argparse.ArgumentParser()
parser.add_argument('--release-dir',required=True)
parser.add_argument('--release',required=True)
parser.add_argument('--control-dir',required=True)
parser.add_argument('--ready-timeout',type=float,default=30)
args=parser.parse_args()
root=pathlib.Path(args.release_dir).resolve(strict=True)
control=pathlib.Path(args.control_dir).resolve();control.mkdir(mode=0o700,parents=True,exist_ok=True);os.chmod(control,0o700)
lock=open(control/'supervisor.lock','w');fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
supervisor_pid=os.getpid(); token=str(uuid.uuid4());child=None
os.umask(0o077)
def stop(signum,frame): raise SystemExit(0)
for sig in (signal.SIGTERM,signal.SIGINT):signal.signal(sig,stop)
def cleanup():
    global child
    if child and child.poll() is None:
        child.send_signal(signal.SIGINT)
        try:child.wait(timeout=5)
        except subprocess.TimeoutExpired:child.kill();child.wait(timeout=5)
def heartbeat(): atomic(control/'owner.json',{'pid':supervisor_pid,'token':token,'heartbeat':int(time.time()*1000)})
env={**os.environ,'AGENTWORKS_SUPERVISOR_DIR':str(control),'AGENTWORKS_SUPERVISOR_TOKEN':token,'AGENTWORKS_RELEASE_DIR':str(root)}
identity=None; previous=None
try:
    selector=control/'active-release.json'
    selected=json.loads(selector.read_text())['release'] if selector.exists() else args.release
    executable=release(root,selected)
    # Stale dispatch tickets are never consumed on supervisor startup.
    (control/'dispatch.json').unlink(missing_ok=True)
    while True:
        heartbeat()
        child=subprocess.Popen([str(executable)],env=env,stdin=subprocess.DEVNULL,preexec_fn=child_setup)
        deadline=time.monotonic()+args.ready_timeout;ready=False
        while child.poll() is None:
            heartbeat()
            if not ready:
                try:
                    port=env.get('AGENTWORKS_PORT','4317')
                    with urllib.request.urlopen('http://127.0.0.1:'+port+'/api/service/restart',timeout=.5) as response: status=json.load(response)
                    ready=status.get('pid')==child.pid and status.get('instanceId')!=previous and status.get('executable')==str(executable) and status.get('restartSupported') is True
                    if ready:
                        previous=status['instanceId']
                        atomic(control/'active-release.json',{'release':executable.name})
                        atomic(control/'outcome.json',{'identity':identity,'phase':'ready','instanceId':previous,'executable':str(executable),'pid':child.pid})
                except (OSError,ValueError): pass
                if not ready and time.monotonic()>deadline: raise RuntimeError('Successor readiness deadline exceeded')
            time.sleep(.15)
        code=child.returncode
        if not ready: raise RuntimeError('Child exited before readiness')
        if code!=75: sys.exit(code if code>=0 else 1)
        with open(control/'dispatch.json') as f: ticket=json.load(f)
        (control/'dispatch.json').unlink()
        if ticket['instance']!=previous or not ticket.get('identity'): raise RuntimeError('Stale or invalid restart ticket')
        target=pathlib.Path(ticket['executable'])
        executable=release(root,target.name)
        if executable!=target:raise ValueError('Release path changed')
        identity=ticket['identity'];env['AGENTWORKS_RESTART_REQUEST']=identity
        atomic(control/'outcome.json',{'identity':identity,'phase':'starting','executable':str(executable)})
        # Exactly one successor per ticket; failed launch/health exits, never retries.
except Exception as error:
    atomic(control/'outcome.json',{'identity':identity,'phase':'failed','detail':str(error)})
    print('Agentworks supervisor:',error,file=sys.stderr)
    sys.exit(1)
finally:
    cleanup()
    (control/'owner.json').unlink(missing_ok=True)
