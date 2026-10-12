#!/usr/bin/env python3
"""Calibrate independent frozen FX08 vectors; controls never reach trial models."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "aethyme/rust/crates/aethyme-testkit/fixtures/fx08"
CONTROLS = {
"01-optional": (
"""pub fn parse_entry(entry: &str, index: usize) -> Result<String,String> {
 let token=entry.trim();
 if token.is_empty() || !token.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c==b'_' || c==b'-') {
 Err(format!("feature entry {index} invalid: {token}"))
 } else { Ok(token.into()) }
}
pub fn parse_feature(setting: Option<&str>) -> Result<Vec<String>,String> {
 let Some(s)=setting else { return Ok(vec!["search".into(),"labels".into()]); };
 if s.is_empty() {return Ok(vec![]);}
 s.split(',').enumerate().map(|(i,s)|parse_entry(s,i)).collect()
}
""",
'if s.is_empty() {return Ok(vec![]);}',
'if s.is_empty() {return Ok(vec!["search".into(),"labels".into()]);}'),
"02-pagination": (
"""#[derive(Clone,Debug,PartialEq,Eq)]
pub struct Entry {pub id:u64,pub priority:i32,pub created:u64}
pub fn ordered(entries:&[Entry],minimum:Option<i32>)->Vec<Entry>{
 let mut result:Vec<_>=entries.iter().filter(|e| minimum.is_none_or(|p|e.priority>=p)).cloned().collect();
 result.sort_by(|a,b|b.priority.cmp(&a.priority).then(a.created.cmp(&b.created)).then(a.id.cmp(&b.id))); result
}
pub fn page(entries:&[Entry],after:Option<(i32,u64,u64)>,limit:usize,minimum:Option<i32>)->Vec<Entry>{
 ordered(entries,minimum).into_iter().filter(|e|after.is_none_or(|(p,c,id)|e.priority<p || (e.priority==p && (e.created,e.id)>(c,id)))).take(limit).collect()
}
""",
'.then(a.created.cmp(&b.created))',''),
"03-protocol": (
"""pub struct Record {pub received:u32,pub id:u32}
impl Record {
 pub fn serialize(&self)->String {format!("recieved={};id={}",self.received,self.id)}
 pub fn describe(&self)->String {format!("Received {} items for record {}",self.received,self.id)}
}
""",
'"recieved={};id={}"','"received={};id={}"'),
"04-completion": (
"""#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum Status {Running,Unknown,Completed,Failed}
pub fn lost_ack()->Status {Status::Unknown}
pub fn may_replay(status:Status)->bool {status==Status::Failed}
pub fn status_text(status:Status)->&'static str {
 match status {Status::Running=>"In progress",Status::Unknown=>"Completion unknown",Status::Completed=>"Completed",Status::Failed=>"Failed"}
}
pub fn notifications(events:&[(u64,Status)])->Vec<(u64,Status)>{
 let mut result=vec![]; for event in events {if !result.contains(event){result.push(*event)}} result
}
""",
'pub fn lost_ack()->Status {Status::Unknown}',
'pub fn lost_ack()->Status {Status::Failed}'),
"05-identifiers": (
"""#[derive(Clone,Debug,PartialEq,Eq)]
pub struct Item {pub raw:String,pub label:String}
pub fn escaped(raw:&str)->String {raw.replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;").replace('"',"&quot;")}
pub fn lookup<'a>(items:&'a [Item],raw:&str)->Option<&'a Item>{items.iter().find(|item|item.raw==raw)}
pub fn display(item:&Item)->String {format!("{} [{}]",escaped(&item.label),escaped(&item.raw))}
pub fn filtered(items:&[Item],needle:&str)->Vec<Item>{items.iter().filter(|item|item.label.contains(needle)).cloned().collect()}
""",
'item.raw==raw','escaped(&item.raw)==raw'),
"06-lookup": (
"""#[derive(Clone,Debug,PartialEq,Eq)]
pub enum Lookup {Ready(Vec<String>),Unavailable}
pub fn fallback(value:&Lookup)->bool {matches!(value,Lookup::Unavailable)}
pub fn render(value:&Lookup)->String {
 match value {Lookup::Ready(items) if items.is_empty()=>"No results".into(),Lookup::Ready(items)=>items.join(", "),Lookup::Unavailable=>"Lookup unavailable".into()}
}
pub fn display_field(value:&Lookup,limit:usize)->String {render(value).chars().take(limit).collect()}
""",
'matches!(value,Lookup::Unavailable)',
'matches!(value,Lookup::Unavailable) || matches!(value,Lookup::Ready(items) if items.is_empty())'),
}

def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def main():
    evidence = Path(sys.argv[1])
    evidence.mkdir()
    report={"schema":"aethyme.fx08.oracle-calibration/v1","state":"planned","manifest_sha256":sha(FIXTURES / "independent-oracle-manifest.json"),"rows":[{"task":task,"kind":kind,"state":"unrun"} for task in CONTROLS for kind in ["positive","survivor-break"]]}
    def persist():
        tmp=evidence / "report.next"
        with tmp.open("w") as f:
            json.dump(report,f,indent=2);f.flush();os.fsync(f.fileno())
        tmp.replace(evidence / "report.json")
    persist()
    env={**os.environ,"RUSTC_WRAPPER":""}
    compiler=shutil.which("rustc")
    if not compiler: raise RuntimeError("rustc missing")
    for task,(positive,old,new) in CONTROLS.items():
        source=next(FIXTURES.glob(f"*/{task}/private/oracle.rs"))
        expected={"01-optional":"54fdd15bdf077d84a2a0c3a9d7a025ed3ac7630e6f5572bc2045249e91835571",
        "02-pagination":"234e5d5ec12e4d619ee7f61fece106ef874705934db02f9dbdd1d68b54995cd6",
        "03-protocol":"c643f771d539f6df742b86d68f581b81890bfa3f67b22ddf4a01af9e80a45c2b",
        "04-completion":"9832b84d367c4309838df62ca282bc626aaff458a53499ac2a142e84f1063636",
        "05-identifiers":"aa5d5f8a5ff2d22d9d33ee1b9303a340aa4589fad2bf05e58453e1389dbb57bb",
        "06-lookup":"4fd892bbf4b85862b354b0050cc2eaca150c1fcf3b0396716b23b57ce3aec171"}[task]
        assert sha(source)==expected,"frozen oracle changed"
        assert positive.count(old)==1,"control edit ambiguous"
        for kind,text in [("positive",positive),("survivor-break",positive.replace(old,new))]:
            root=evidence / f"{task}-{kind}"; root.mkdir()
            shutil.copyfile(source,root / "oracle.rs"); (root / "source.rs").write_text(text)
            row=next(row for row in report["rows"] if row["task"]==task and row["kind"]==kind)
            row.update({"state":"admitted","oracle_sha256":expected,"source_sha256":sha(root / "source.rs")});persist()
            try:
                compile_result=subprocess.run([compiler,"--edition=2021","--test","oracle.rs","-o","oracle"],cwd=root,env=env,capture_output=True,timeout=30)
                (root / "compile.stdout").write_bytes(compile_result.stdout);(root / "compile.stderr").write_bytes(compile_result.stderr)
                row["compile_exit"]=compile_result.returncode
                if compile_result.returncode: raise RuntimeError("calibration compile failure is not a useful negative")
                run=subprocess.run([str(root / "oracle"),"--nocapture"],cwd=root,env=env,capture_output=True,timeout=15)
                (root / "run.stdout").write_bytes(run.stdout);(root / "run.stderr").write_bytes(run.stderr)
                row["exit"]=run.returncode
                row["named_survivor_failure"]=any("test survives_" in line and "FAILED" in line for line in run.stdout.decode().splitlines())
                row["accepted"]=run.returncode==0 if kind=="positive" else run.returncode==101 and row["named_survivor_failure"]
                row["state"]="completed"
                persist()
                assert row["accepted"],f"control did not distinguish {task}-{kind}"
            except BaseException:
                if row["state"]=="admitted": row["state"]="unknown"
                report["state"]="failed";persist();raise
    report["state"]="completed";persist()
    print(json.dumps({"controls":len(report["rows"]),"state":report["state"],"evidence":str(evidence)}))
if __name__=="__main__": main()

