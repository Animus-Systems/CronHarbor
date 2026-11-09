#!/usr/bin/env python3
"""Finite real user-systemd drill; selected binary, private state and owned units only."""
import argparse,hashlib,json,os,signal,subprocess,tempfile,time,uuid
from pathlib import Path

def main():
 parser=argparse.ArgumentParser();parser.add_argument('--binary',type=Path,required=True);args=parser.parse_args()
 binary=args.binary.resolve(strict=True);assert os.getuid()!=0,'Use the user manager, not root'
 tag='cronharbor-proof-'+uuid.uuid4().hex[:12]
 units=Path.home()/'.config/systemd/user';created=[];daemon=None;log=None
 def system(*values):return subprocess.check_output(['systemctl','--user',*values],text=True,timeout=15).strip()
 def wait(action,seconds=15):
  deadline=time.monotonic()+seconds
  while time.monotonic()<deadline:
   value=action()
   if value:return value
   time.sleep(.05)
  raise AssertionError('Disposable systemd condition did not complete')
 with tempfile.TemporaryDirectory(prefix=tag+'-',dir=Path.home()/'.local/state') as temporary:
  root=Path(temporary);root.chmod(0o700);state=root/'state';state.mkdir(mode=0o700)
  worker=root/'worker.py';worker.write_text('from pathlib import Path\nimport sys,time\np=Path(sys.argv[1]);p.write_text(str(int(p.read_text())+1 if p.exists() else 1));time.sleep(float(Path(sys.argv[2]).read_text()))\n');worker.chmod(0o600)
  for label in ['job','service']:
   (root/(label+'.duration')).write_text('0' if label=='job' else '300')
   name=tag+'-'+label+'.service';path=units/name;assert not path.exists()
   body='[Unit]\nDescription=Disposable Cron Harbor executor proof\n[Service]\nType='+('oneshot' if label=='job' else 'simple')+'\nExecStart=/usr/bin/python3 '+str(worker)+' '+str(root/(label+'.effects'))+' '+str(root/(label+'.duration'))+'\nNoNewPrivileges=true\nMemoryMax=64M\nTasksMax=16\nTimeoutStopSec=5\n'
   path.write_text(body);path.chmod(0o600);created.append((name,path,hashlib.sha256(body.encode()).hexdigest()))
  job_unit,service_unit=[row[0] for row in created]
  policy={'systemdEnabled':True,'scopedUnits':[{'unit':name,'scope':'user'} for name,_,_ in created]}
  path=state/'executors.json';path.write_text(json.dumps(policy));path.chmod(0o600)
  def cli(*values,check=True):
   r=subprocess.run([str(binary),'--state-dir',str(state),*values],capture_output=True,text=True,timeout=15)
   if check:assert r.returncode==0,'Fixture CLI failed: '+r.stderr
   return json.loads(r.stdout) if r.returncode==0 else None
  def boot():
   nonlocal daemon,log
   log=(root/'daemon.log').open('a');daemon=subprocess.Popen([str(binary),'--state-dir',str(state),'server'],stdout=log,stderr=subprocess.STDOUT)
   wait(lambda:daemon.poll() is None and (state/'control.sock').exists() and cli('status',check=False))
  def stop():
   nonlocal daemon,log
   if daemon and daemon.poll() is None:
    daemon.send_signal(signal.SIGTERM)
    try:daemon.wait(timeout=10)
    except subprocess.TimeoutExpired:daemon.kill();daemon.wait(timeout=5);raise AssertionError('Fixture daemon did not stop')
   daemon=None
   if log:log.close();log=None
  def definition(name,kind,unit):
   return {'apiVersion':'cronharbor.dev/v1','kind':kind,'metadata':{'name':name,'namespace':'proof'},'spec':{'owner':'file','enabled':True,'execution':{'adapter':'systemd','systemd':{'unit':unit}},'concurrency':{'mode':'forbid','maxActive':1},'timeoutSeconds':40,'retry':{'maxAttempts':1},'service':{'desiredState':'STOPPED','restart':'never','stopGraceSeconds':5}}}
  def apply(name,kind,unit):
   p=root/(name+'.json');p.write_text(json.dumps(definition(name,kind,unit)));cli('config','apply','--file',str(p))
  def current(name):
   return next(row for row in cli('jobs','list')['items'] if row['name']==name)
  def run_job():
   operation=cli('jobs','run','proof/job','--idempotency-key',str(uuid.uuid4()))
   def completed():
    value=cli('operations','inspect',operation['id']);return value if value['state'] in ['SUCCEEDED','FAILED','UNKNOWN'] else None
   result=wait(completed,25);assert result['state']=='SUCCEEDED',result
  try:
   system('daemon-reload');boot();apply('job','OneShotJob',job_unit);apply('service','Service',service_unit)
   run_job();assert (root/'job.effects').read_text()=='1'
   (root/'job.duration').write_text('16');start=time.monotonic();operation=cli('jobs','run','proof/job','--idempotency-key',str(uuid.uuid4()))
   wait(lambda:system('show',job_unit,'--property=ActiveState','--value')=='activating')
   assert cli('operations','inspect',operation['id'])['state'] in ['ACCEPTED','RUNNING']
   wait(lambda:cli('operations','inspect',operation['id'])['state']=='SUCCEEDED',25);assert time.monotonic()-start>=16
   assert (root/'job.effects').read_text()=='2'
   system('start',service_unit);wait(lambda:(root/'service.effects').exists());pid=system('show',service_unit,'--property=MainPID','--value');invocation=system('show',service_unit,'--property=InvocationID','--value')
   cli('services','start','proof/service');wait(lambda:current('service')['observed_state']=='READY')
   assert system('show',service_unit,'--property=MainPID','--value')==pid and system('show',service_unit,'--property=InvocationID','--value')==invocation
   assert (root/'service.effects').read_text()=='1'
   stop();assert system('show',service_unit,'--property=MainPID','--value')==pid;boot();wait(lambda:current('service')['observed_state']=='READY')
   assert system('show',service_unit,'--property=MainPID','--value')==pid and (root/'service.effects').read_text()=='1'
   operation=cli('services','stop','proof/service');wait(lambda:cli('operations','inspect',operation['id'])['state']=='SUCCEEDED')
   assert system('show',service_unit,'--property=ActiveState','--value')=='inactive'
   assert cli('status')['unknown']==0
   print(json.dumps({'check':'real-user-systemd','status':'pass','fastJob':True,'longOneshotSeconds':16,'serviceAdoptedWithoutRestart':True,'ownerRestartReattached':True,'serviceStoppedWithReceipt':True,'unknown':0}))
  finally:
   try:stop()
   finally:
    for name,path,digest in created:
     assert path.name.startswith(tag) and hashlib.sha256(path.read_bytes()).hexdigest()==digest
     system('stop',name);path.unlink()
    system('daemon-reload')
if __name__=='__main__':main()
