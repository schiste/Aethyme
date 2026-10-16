from pathlib import Path
import subprocess,os,json
w=Path('/Volumes/T7-repositories/Application Support/Aethyme/worktrees/aethyme-6f13a03e16f294c6/complete-exact-immutable-contribution-co');r=Path('/Volumes/T7-repositories/Application Support/Aethyme/worktrees/aethyme-6f13a03e16f294c6/682-reconcile-final-local-qualification/target/local-resume/record-final-local-check-v2.py');env=os.environ.copy();env['PATH']='/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/Users/christophehenner/.cargo/bin';env['RUSTC_WRAPPER']=''
for name,selector in [('restored-context','collaboration_context::revisions::tests'),('restored-entry-budget','collaboration_identity_store::replay_budget_tests')]:
 args=['/usr/bin/python3',str(r),'--root',str(w),'--cwd','packages/aethyme/rust','--tmp-base','/private/tmp','--output-root','target/local-resume/exact-revision-checks','--label','exact-controls-restored-a6b34828-'+name,'--','cargo','test','--locked','-j4','-p','aethyme-broker','--lib',selector,'--','--test-threads=1','--nocapture']
 run=subprocess.run(args,cwd=w,env=env,capture_output=True,text=True);print(json.dumps({'name':name,'exit':run.returncode,'stdout':run.stdout,'stderr':run.stderr}),flush=True)
 events=[json.loads(x) for x in run.stdout.splitlines() if x.startswith('{')];final=events[-1] if events else {};log=Path(final['log']).read_text() if 'log' in final else ''
 if run.returncode and 'running ' not in log:print('STOP: no tests executed',flush=True);break
