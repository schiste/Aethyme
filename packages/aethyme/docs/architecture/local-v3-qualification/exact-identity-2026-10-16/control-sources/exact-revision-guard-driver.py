from pathlib import Path
import os, subprocess, tempfile, json, hashlib
w=Path('/Volumes/T7-repositories/Application Support/Aethyme/worktrees/aethyme-6f13a03e16f294c6/complete-exact-immutable-contribution-co')
r=Path('/Volumes/T7-repositories/Application Support/Aethyme/worktrees/aethyme-6f13a03e16f294c6/682-reconcile-final-local-qualification/target/local-resume/record-final-local-check-v2.py')
c=w/'packages/aethyme/rust/crates/aethyme-broker/src/collaboration_context/revisions.rs'
i=w/'packages/aethyme/rust/crates/aethyme-broker/src/collaboration_identity_store.rs'
originals={p:p.read_bytes() for p in [c,i]}
backup=Path(tempfile.mkdtemp(prefix='root-exact-revision-guards-',dir='/private/tmp'))
for p,b in originals.items(): (backup/p.name).write_bytes(b)
trials=[
 ('conflicting-pin',c,'&& previous != pin','&& previous != pin && false','collaboration_context::revisions::tests::exact_context_refuses_conflicting_pin_and_changed_visibility_epoch'),
 ('visibility-epoch',c,'|| visibility.epoch() != epoch','|| (visibility.epoch() == u64::MAX && epoch == u64::MAX)','collaboration_context::revisions::tests::exact_context_refuses_conflicting_pin_and_changed_visibility_epoch'),
 ('dependency-gap-recompute',c,'!gap.starts_with("missing_prerequisite:")\n                && !gap.starts_with("prerequisite_not_returned:")','!gap.starts_with("missing_prerequisite:")\n                && !gap.starts_with("prerequisite_not_returned:") || true','collaboration_context::revisions::tests::exact_context_dependency_gaps_follow_selected_items_and_byte_budget'),
 ('persist-base-proofs',i,'let identity_base_revision_ids = provenance_ids(base_state.provenance());','let identity_base_revision_ids: BTreeSet<RecordId> = BTreeSet::new();','collaboration_context::revisions::tests::exact_revision_pin_keeps_ancestor_proofs_then_gc_reclaims_unrooted_history'),
 ('grace-exact-pin-seed',i,'if revision_ids.insert(revision_id.clone()) {','if false && revision_ids.insert(revision_id.clone()) {','collaboration_context::revisions::tests::exact_pin_reads_unrooted_history_during_grace_without_creating_retention'),
 ('identity-entry-budget',i,'self.identity_entry_units_remaining -= units;','self.identity_entry_units_remaining -= units.saturating_sub(units);','collaboration_identity_store::replay_budget_tests::refuses_identity_map_allocation_before_advancing_over_budget'),
]
results=[]
try:
 for label,p,old,new,test in trials:
  raw=originals[p].decode(); assert raw.count(old)==1,(label,raw.count(old));p.write_text(raw.replace(old,new,1))
  argv=['/usr/bin/python3',str(r),'--root',str(w),'--cwd','packages/aethyme/rust','--tmp-base','/private/tmp','--output-root','target/local-resume/exact-revision-checks','--label','exact-revision-neuter-'+label,'--','cargo','test','--locked','-j4','-p','aethyme-broker','--lib',test,'--','--exact','--test-threads=1','--nocapture']
  env=os.environ.copy();env['PATH']='/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/Users/christophehenner/.cargo/bin';env['RUSTC_WRAPPER']=''
  run=subprocess.run(argv,cwd=w,env=env,capture_output=True,text=True)
  (backup/(label+'.stdout')).write_text(run.stdout);(backup/(label+'.stderr')).write_text(run.stderr)
  print(json.dumps({'label':label,'recorder_exit':run.returncode,'stdout':run.stdout,'stderr':run.stderr}),flush=True)
  results.append({'label':label,'exit':run.returncode});p.write_bytes(originals[p])
finally:
 for p,b in originals.items(): p.write_bytes(b);assert p.read_bytes()==b
 (backup/'summary.json').write_text(json.dumps({'restored':{p.name:hashlib.sha256(b).hexdigest() for p,b in originals.items()},'trials':results},indent=2)+'\n')
 print('RESTORED '+str(backup),flush=True)
