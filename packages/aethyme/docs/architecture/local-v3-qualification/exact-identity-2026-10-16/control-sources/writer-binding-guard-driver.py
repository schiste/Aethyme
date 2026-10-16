from pathlib import Path
import os,subprocess,tempfile,json,hashlib,re,sys,signal
w=Path('/Volumes/T7-repositories/Application Support/Aethyme/worktrees/aethyme-6f13a03e16f294c6/complete-exact-immutable-contribution-co')
r=Path('/Volumes/T7-repositories/Application Support/Aethyme/worktrees/aethyme-6f13a03e16f294c6/682-reconcile-final-local-qualification/target/local-resume/record-final-local-check-v2.py')
i=w/'packages/aethyme/rust/crates/aethyme-broker/src/collaboration_identity_store.rs'
p=w/'packages/aethyme/rust/crates/aethyme-broker/src/collaboration_cli/project.rs'
head=subprocess.check_output(['/opt/homebrew/bin/git','rev-parse','HEAD'],cwd=w,text=True).strip();assert len(sys.argv)==2 and sys.argv[1]==head,'explicit exact source HEAD required'
assert not subprocess.check_output(['/opt/homebrew/bin/git','status','--porcelain'],cwd=w,text=True),'source must be clean'
originals={path:path.read_bytes() for path in (i,p)}
backup=Path(tempfile.mkdtemp(prefix='root-writer-binding-guards-',dir='/private/tmp'))
for path,raw in originals.items(): (backup/path.name).write_bytes(raw)
(backup/'source.json').write_text(json.dumps({'head':head,'sources':{str(path.relative_to(w)):hashlib.sha256(raw).hexdigest() for path,raw in originals.items()}},indent=2)+'\n')
trials=[
 ('writer-lock-lifetime',i,'let _writer = acquire_revision_writer_lock(store)?;','let writer = acquire_revision_writer_lock(store)?; drop(writer);','collaboration_identity_store::identity_store_tests::revision_writers_serialize_preflight_and_refuse_a_stale_dependent_capture','the competing writer must own the lock while paused after preflight'),
 ('corrupt-binding-refusal',p,'if matches!(error, identity_store::IdentityStoreError::Corrupt(_)) {','if matches!(error, identity_store::IdentityStoreError::Corrupt(_)) && false {','collaboration_cli::project::binding_tests::a_corrupt_bootstrap_transition_is_a_typed_binding_refusal','assertion `left == right` failed')]
env=os.environ.copy();env['PATH']='/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/Users/christophehenner/.cargo/bin';env['RUSTC_WRAPPER']=''
rows=[]
def terminate_into_restore(signum, frame):
 raise KeyboardInterrupt('termination requested: '+str(signum))
signal.signal(signal.SIGTERM, terminate_into_restore)
try:
 for label,p,old,new,test,intended_assertion in trials:
  raw=originals[p].decode();assert raw.count(old)==1,(label,raw.count(old));p.write_text(raw.replace(old,new,1))
  argv=['/usr/bin/python3',str(r),'--root',str(w),'--cwd','packages/aethyme/rust','--tmp-base','/private/tmp','--output-root','target/local-resume/exact-revision-checks','--label','writer-binding-neuter-'+label,'--','cargo','test','--locked','-j4','-p','aethyme-broker','--lib',test,'--','--exact','--test-threads=1','--nocapture']
  run=subprocess.run(argv,cwd=w,env=env,capture_output=True,text=True);(backup/(label+'.stdout')).write_text(run.stdout);(backup/(label+'.stderr')).write_text(run.stderr)
  events=[json.loads(l) for l in run.stdout.splitlines() if l.startswith('{')];final=events[-1] if events else {};log=Path(final['log']).read_text() if 'log' in final else '';summaries=re.findall(r'test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored',log)
  row={'label':label,'exit':run.returncode,'final':final,'test_results':summaries};rows.append(row);print(json.dumps(row),flush=True);p.write_bytes(originals[p])
  if not(run.returncode==101 and summaries==[('0','1','0')] and 'panicked at ' in log and intended_assertion in log):raise RuntimeError('intended executed test failure was not established')
finally:
 for path,raw in originals.items():
  path.write_bytes(raw);assert path.read_bytes()==raw
 (backup/'summary.json').write_text(json.dumps({'source_restored':True,'sources':{str(path.relative_to(w)):hashlib.sha256(raw).hexdigest() for path,raw in originals.items()},'trials':rows},indent=2)+'\n');print('RESTORED '+str(backup),flush=True)
