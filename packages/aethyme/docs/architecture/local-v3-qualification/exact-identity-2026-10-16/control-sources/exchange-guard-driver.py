from pathlib import Path
import os,subprocess,tempfile,json,hashlib,re,sys
w=Path('/Volumes/T7-repositories/Application Support/Aethyme/worktrees/aethyme-6f13a03e16f294c6/complete-exact-immutable-contribution-co')
r=Path('/Volumes/T7-repositories/Application Support/Aethyme/worktrees/aethyme-6f13a03e16f294c6/682-reconcile-final-local-qualification/target/local-resume/record-final-local-check-v2.py')
p=w/'packages/aethyme/rust/crates/aethyme-broker/src/local_exchange.rs'
head=subprocess.check_output(['/opt/homebrew/bin/git','rev-parse','HEAD'],cwd=w,text=True).strip();assert len(sys.argv)==2 and sys.argv[1]==head,'explicit exact source HEAD required'
assert not subprocess.check_output(['/opt/homebrew/bin/git','status','--porcelain'],cwd=w,text=True),'source must be clean'
testpath=w/'packages/aethyme/rust/crates/aethyme-broker/src/local_exchange/revision_tests.rs'
assert 'short_nonfinal_over_inline_threshold' in testpath.read_text(),'isolated chunk-shape regression must be present'
original=p.read_bytes();backup=Path(tempfile.mkdtemp(prefix='root-exchange-guards-',dir='/private/tmp'));(backup/p.name).write_bytes(original);(backup/'source.json').write_text(json.dumps({'head':head,'test_sha256':hashlib.sha256(testpath.read_bytes()).hexdigest(),'source_sha256':hashlib.sha256(original).hexdigest()},indent=2)+'\n')
trials=[('full-prefix-chunk-shape','if index + 1 < chunks.len() && chunk.len() != MAX_REVISION_HEX_CHUNK_CHARS {','if index + 1 < chunks.len() && chunk.len() != MAX_REVISION_HEX_CHUNK_CHARS && false {','revision_hex_chunks_require_one_bounded_canonical_shape'),('whole-envelope-preflight','if total > self.limit {','if total > self.limit && false {','complete_event_wire_size_is_preflighted_under_the_generic_record_cap'),('aggregate-page-byte-cap','if total_bytes > MAX_SNAPSHOT_PAGE_BYTES {','if total_bytes > MAX_SNAPSHOT_PAGE_BYTES && false {','snapshot_page_rejects_aggregate_revision_bytes_over_the_page_cap')]
env=os.environ.copy();env['PATH']='/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/Users/christophehenner/.cargo/bin';env['RUSTC_WRAPPER']=''
rows=[]
try:
 for label,old,new,test in trials:
  raw=original.decode();assert raw.count(old)==1,(label,raw.count(old));p.write_text(raw.replace(old,new,1))
  argv=['/usr/bin/python3',str(r),'--root',str(w),'--cwd','packages/aethyme/rust','--tmp-base','/private/tmp','--output-root','target/local-resume/exact-revision-checks','--label','exchange-neuter-'+label,'--','cargo','test','--locked','-j4','-p','aethyme-broker','--lib','local_exchange::revision_tests::'+test,'--','--exact','--test-threads=1','--nocapture']
  run=subprocess.run(argv,cwd=w,env=env,capture_output=True,text=True);(backup/(label+'.stdout')).write_text(run.stdout);(backup/(label+'.stderr')).write_text(run.stderr)
  events=[json.loads(l) for l in run.stdout.splitlines() if l.startswith('{')];final=events[-1] if events else {};log=Path(final['log']).read_text() if 'log' in final else '';summaries=re.findall(r'test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored',log)
  row={'label':label,'exit':run.returncode,'final':final,'test_results':summaries};rows.append(row);print(json.dumps(row),flush=True);p.write_bytes(original)
  if not(run.returncode==101 and summaries==[('0','1','0')] and 'panicked at ' in log):print('STOP: intended executed test failure was not established',flush=True);break
finally:
 p.write_bytes(original);assert p.read_bytes()==original
 (backup/'summary.json').write_text(json.dumps({'source_restored':True,'source_sha256':hashlib.sha256(original).hexdigest(),'trials':rows},indent=2)+'\n');print('RESTORED '+str(backup),flush=True)
