from pathlib import Path
import os, subprocess, tempfile, json, hashlib
w=Path('/Volumes/T7-repositories/Application Support/Aethyme/worktrees/aethyme-6f13a03e16f294c6/complete-exact-immutable-contribution-co');r=Path('/Volumes/T7-repositories/Application Support/Aethyme/worktrees/aethyme-6f13a03e16f294c6/682-reconcile-final-local-qualification/target/local-resume/record-final-local-check-v2.py');p=w/'packages/aethyme/rust/crates/aethyme-broker/src/collaboration_gc.rs'
before=p.read_bytes(); backup=Path(tempfile.mkdtemp(prefix='root-gc-reviewed-guards-',dir='/private/tmp'));(backup/'original.rs').write_bytes(before)
trials=[('root-budget','*remaining -= 1;','*remaining = remaining.saturating_sub(0);','identity_root_discovery_budget_is_shared_and_stops_enumeration'),('failed-read-byte-budget','*remaining -= length;','*remaining = remaining.saturating_sub(length.saturating_sub(length));','identity_corrupt_manifest_is_charged_and_stops_discovery'),('streaming-union','IS NULL UNION ALL SELECT kind,reference FROM local_controller_completion_roots','IS NULL UNION SELECT kind,reference FROM local_controller_completion_roots','identity_native_root_query_streams_before_the_collector_limit')]
results=[]
try:
 for label,old,new,test in trials:
  s=before.decode(); assert s.count(old)==1,(label,s.count(old));p.write_text(s.replace(old,new,1))
  argv=['/usr/bin/python3',str(r),'--root',str(w),'--cwd','packages/aethyme/rust','--tmp-base','/private/tmp','--output-root','target/local-resume/exact-revision-checks','--label','identity-gc-reviewed-neuter-'+label,'--','cargo','test','--locked','-j4','-p','aethyme-broker','--lib','collaboration_gc::tests::'+test,'--','--exact','--test-threads=1','--nocapture']
  env=os.environ.copy();env['PATH']='/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/Users/christophehenner/.cargo/bin';env['RUSTC_WRAPPER']=''
  run=subprocess.run(argv,cwd=w,env=env,capture_output=True,text=True);(backup/(label+'.stdout')).write_text(run.stdout);(backup/(label+'.stderr')).write_text(run.stderr)
  print(json.dumps({'label':label,'recorder_exit':run.returncode,'stdout':run.stdout,'stderr':run.stderr}),flush=True);results.append({'label':label,'exit':run.returncode});p.write_bytes(before)
finally:
 p.write_bytes(before);assert p.read_bytes()==before;(backup/'summary.json').write_text(json.dumps({'restored_sha256':hashlib.sha256(before).hexdigest(),'trials':results},indent=2)+'\n');print('RESTORED '+str(backup),flush=True)
