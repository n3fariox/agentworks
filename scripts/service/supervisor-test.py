#!/usr/bin/env python3
"""Deterministic fake-server checks of the external supervisor contract, isolated port 4442."""
import json, os, pathlib, signal, subprocess, tempfile, time, unittest
SUPERVISOR=pathlib.Path(__file__).with_name('supervise.py').resolve()
FAKE=r'''#!/usr/bin/python3
import http.server,json,os,pathlib,threading,time
root=pathlib.Path(os.environ['FIXTURE']);count=root/'count';n=int(count.read_text())+1 if count.exists() else 1;count.write_text(str(n));(root/'pid').write_text(str(os.getpid()))
mode=os.environ['MODE'];control=pathlib.Path(os.environ['AGENTWORKS_SUPERVISOR_DIR']);instance='fake-'+str(n)
class Handler(http.server.BaseHTTPRequestHandler):
 def log_message(self,*args):pass
 def do_GET(self):
  self.send_response(200);self.end_headers();self.wfile.write(json.dumps({'pid':os.getpid()+(1 if mode=='wrongpid' else 0),'instanceId':instance,'executable':str(pathlib.Path(__file__).resolve()),'restartSupported':True}).encode())
def finish():
 time.sleep(.7)
 if mode=='restart' and n==1:
  (control/'dispatch.json').write_text(json.dumps({'instance':instance,'identity':'fake:one','executable':str(pathlib.Path(__file__).resolve())}));os._exit(75)
 if mode=='stale':
  (control/'dispatch.json').write_text(json.dumps({'instance':'stale','identity':'fake:stale','executable':str(pathlib.Path(__file__).resolve())}));os._exit(75)
 if mode not in ('hang','unready','wrongpid'):os._exit(0)
threading.Thread(target=finish,daemon=True).start()
if mode=='unready':time.sleep(10)
else:
 http.server.HTTPServer.allow_reuse_address=True
 http.server.HTTPServer(('127.0.0.1',4442),Handler).serve_forever()
'''
class Contract(unittest.TestCase):
 def run_case(self,mode):
  with tempfile.TemporaryDirectory(prefix='agentworks-supervisor-') as d:
   root=pathlib.Path(d);release=root/'releases';release.mkdir();fake=release/'fake';fake.write_text(FAKE);fake.chmod(0o700)
   p=subprocess.Popen(['/usr/bin/python3',str(SUPERVISOR),'--release-dir',str(release),'--release','fake','--control-dir',str(root/'control'),'--ready-timeout','1.5'],env={**os.environ,'FIXTURE':d,'MODE':mode,'AGENTWORKS_PORT':'4442'},stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
   try:
    if mode=='hang':
     end=time.monotonic()+4
     while not (root/'pid').exists() and time.monotonic()<end:time.sleep(.05)
     pid=int((root/'pid').read_text());p.kill();p.wait(timeout=3)
     for _ in range(40):
      try: state=pathlib.Path('/proc',str(pid),'stat').read_text().split()[2]
      except FileNotFoundError:break
      if state=='Z':break
      time.sleep(.05)
     else:self.fail('Child survived killed supervisor')
     return
    code=p.wait(timeout=7);n=int((root/'count').read_text());out=json.loads((root/'control/outcome.json').read_text())
    if mode=='restart':self.assertEqual(n,2);self.assertEqual(code,0);self.assertEqual(out['identity'],'fake:one');self.assertEqual(out['instanceId'],'fake-2')
    elif mode=='normal':self.assertEqual(n,1);self.assertEqual(code,0)
    else:self.assertEqual(n,1);self.assertNotEqual(code,0);self.assertEqual(out['phase'],'failed')
   finally:
    if p.poll() is None:p.terminate();p.wait(timeout=7)
 def test_normal_exit_does_not_loop(self):self.run_case('normal')
 def test_exactly_one_successor_per_ticket(self):self.run_case('restart')
 def test_stale_ticket_refused(self):self.run_case('stale')
 def test_readiness_failure_bounded(self):self.run_case('unready')
 def test_wrong_process_cannot_ack_readiness(self):self.run_case('wrongpid')
 def test_supervisor_crash_kills_child(self):self.run_case('hang')
if __name__=='__main__':unittest.main()
