import datetime,hashlib,json,os,subprocess,tempfile
from pathlib import Path
r=Path.cwd(); src=r/"packages/aethyme/rust/crates/aethyme-broker/src/native_gates.rs"
original=src.read_bytes(); needle=b"            command.env_remove(key);"
assert original.count(needle)==1
backup=Path(tempfile.mkdtemp(prefix="native-child-mutation-",dir="/private/tmp"))/"native_gates.rs"
backup.write_bytes(original)
record={"backup":str(backup),"before_sha256":hashlib.sha256(original).hexdigest(),"started_at_utc":datetime.datetime.now(datetime.timezone.utc).isoformat()}
try:
 src.write_bytes(original.replace(needle,b'            command.env_remove("AETHYME_NATIVE_TEST_UNUSED_KEY");'))
 record["mutant_sha256"]=hashlib.sha256(src.read_bytes()).hexdigest()
 cmd=["/usr/bin/python3",str(r/"target/local-resume/record-final-local-check-v2.py"),"--root",str(r),"--label","native-git-child-no-strip-negative","--cwd","packages/aethyme/rust","--output-root","target/local-resume/native-git-child-checks","--tmp-base","/private/tmp","--","/usr/bin/env","GIT_CONFIG_GLOBAL=/dev/null","GIT_CONFIG_NOSYSTEM=1","GIT_CONFIG_COUNT=1","GIT_CONFIG_KEY_0=user.useConfigOnly","GIT_CONFIG_VALUE_0=true","cargo","test","--locked","-j","4","-p","aethyme-broker","--lib","native_gates::tests::no_gate_configuration_is_explicitly_not_verification","--","--exact","--nocapture"]
 result=subprocess.run(cmd,check=False);record["recorder_exit"]=result.returncode
 assert result.returncode==101, "mutation must produce a real failing test"
 runs=list((r/"target/local-resume/native-git-child-checks").glob("*-native-git-child-no-strip-negative-*/command.log"));assert len(runs)==1
 log=runs[0].read_text();assert "isolated native-gate child must not inherit any GIT_* variable" in log and "test result: FAILED. 0 passed; 1 failed;" in log
 record["expected_named_failure"]=True
finally:
 src.write_bytes(backup.read_bytes());record["restored_sha256"]=hashlib.sha256(src.read_bytes()).hexdigest();record["exact_restore"]=src.read_bytes()==original
 record["finished_at_utc"]=datetime.datetime.now(datetime.timezone.utc).isoformat()
 (r/"target/local-resume/native-git-child-mutation-result.json").write_text(json.dumps(record,indent=2)+"\n")
 assert record["exact_restore"]
