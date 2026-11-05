#!/usr/bin/env python3
"""Compare pinned allocator builds on disposable source-only Playgrounds.

Run on macOS or Linux. Builds are supplied, never compiled by this runner.
Raw stdout/stderr, wait4 CPU/RSS, argv, hashes and semantic checks are retained.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import select
import signal
import socket
import statistics
import subprocess
import tempfile
import time

ARMS = ("default", "rusty", "mimalloc")


def digest(value):
    return hashlib.sha256(value).hexdigest()


def dump(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def tree_hash(root):
    """Hash names and bytes, excluding only Git administrative files."""
    h = hashlib.sha256()
    for p in sorted(root.rglob("*")):
        if ".git" in p.relative_to(root).parts or not p.is_file():
            continue
        name = p.relative_to(root).as_posix().encode()
        data = p.read_bytes()
        h.update(len(name).to_bytes(8, "big") + name)
        h.update(len(data).to_bytes(8, "big") + data)
    return h.hexdigest()


def environment(home):
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("GIT_", "AETHYME_", "MIMALLOC_", "RUSTY_ALLOC_"))}
    env.update(HOME=str(home), XDG_CACHE_HOME=str(home/"cache"),
               XDG_CONFIG_HOME=str(home/"config"), XDG_STATE_HOME=str(home/"state"),
               AETHYME_HOST_CACHE_DIR=str(home/"host-cache"),
               AETHYME_HOST_STATE_DIR=str(home/"host-state"),
               GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null",
               GIT_AUTHOR_NAME="Allocator benchmark", GIT_COMMITTER_NAME="Allocator benchmark",
               GIT_AUTHOR_EMAIL="benchmark@example.invalid", GIT_COMMITTER_EMAIL="benchmark@example.invalid",
               GIT_AUTHOR_DATE="2026-11-05T00:00:00Z", GIT_COMMITTER_DATE="2026-11-05T00:00:00Z",
               RAYON_NUM_THREADS="4", CHAU7_CTO_OPTIM_ACTIVE="1")
    return env


def exit_ready(pid, timeout):
    if hasattr(os, "pidfd_open"):
        fd = os.pidfd_open(pid)
        try:
            return bool(select.select([fd], [], [], timeout)[0])
        finally:
            os.close(fd)
    if hasattr(select, "kqueue"):
        queue = select.kqueue()
        try:
            event = select.kevent(pid, filter=select.KQ_FILTER_PROC,
                                 flags=select.KQ_EV_ADD | select.KQ_EV_ONESHOT,
                                 fflags=select.KQ_NOTE_EXIT)
            try:
                return bool(queue.control([event], 1, timeout))
            except ProcessLookupError:
                return True
        finally:
            queue.close()
    raise RuntimeError("requires pidfd_open or kqueue for timed process accounting")


class Runner:
    def __init__(self, output):
        self.output = output
        self.rows = []

    def run(self, label, argv, cwd, env, timeout=300):
        stem = self.output/f"{len(self.rows):04d}-{label}"
        stdout = stem.with_suffix(".stdout")
        stderr = stem.with_suffix(".stderr")
        start = time.monotonic()
        load = os.getloadavg()
        with stdout.open("xb") as out, stderr.open("xb") as err:
            p = subprocess.Popen([str(x) for x in argv], cwd=cwd, env=env,
                                 stdout=out, stderr=err, start_new_session=True)
            # Wait for exit readiness, avoiding a polling interval in short CLI timings.
            # The child remains unreaped until after any timeout signal, so its PID
            # cannot be recycled while this code still owns the process group.
            try:
                ready = exit_ready(p.pid, timeout)
            except BaseException:
                try:
                    os.killpg(p.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                _, status, _ = os.wait4(p.pid, 0)
                p.returncode = os.waitstatus_to_exitcode(status)
                raise
            expired = not ready
            if expired:
                os.killpg(p.pid, signal.SIGKILL)
            pid, status, usage = os.wait4(p.pid, 0)
            p.returncode = os.waitstatus_to_exitcode(status)
        row = dict(label=label, argv=[str(x) for x in argv], cwd=str(cwd),
                   wall_s=time.monotonic()-start, user_s=usage.ru_utime,
                   system_s=usage.ru_stime, peak_rss_bytes=usage.ru_maxrss*(1 if platform.system()=="Darwin" else 1024),
                   exit_code=p.returncode, timeout=expired, load_before=load,
                   stdout=str(stdout), stderr=str(stderr))
        self.rows.append(row)
        with (self.output/"commands.jsonl").open("a") as f:
            f.write(json.dumps(row)+"\n")
        if p.returncode or expired:
            raise RuntimeError(f"{label} failed: {row}; {stderr.read_text()[-4000:]}")
        return row, stdout.read_text()

    def git(self, repo, env, *args):
        return self.run("git", ["git", "-c", "maintenance.auto=false", "-c", "gc.auto=0", *args], repo, env)[1].strip()


def semantic(value):
    """Only the documented timing/RSS diagnostics are excluded."""
    if isinstance(value, dict):
        return {k: semantic(v) for k, v in value.items()
                if k not in {"elapsed_ms", "index_elapsed_ms", "performance"}
                and not k.endswith("_elapsed_us")}
    if isinstance(value, list):
        return [semantic(v) for v in value]
    return value


def socket_request(path, request):
    with socket.socket(socket.AF_UNIX) as s:
        s.settimeout(120)
        s.connect(str(path))
        s.sendall(json.dumps(request).encode()+b"\n")
        data = b""
        while True:
            part = s.recv(65536)
            if not part:
                break
            data += part
            if len(data) > 16*1024*1024:
                raise RuntimeError("daemon response exceeded 16 MiB")
    result = json.loads(data)
    if result.get("ok") is not True:
        raise RuntimeError(f"daemon refused request: {result}")
    return result


def rss(pid):
    return int(subprocess.check_output(["ps", "-o", "rss=", "-p", str(pid)], text=True).strip())*1024


def daemon_probe(binary, repo, env, output, requests):
    with tempfile.TemporaryDirectory(prefix="alloc-socket-", dir="/private/tmp" if platform.system()=="Darwin" else None) as tmp:
        env = dict(env, AETHYME_ENGINE_SOCKET_DIR=tmp)
        argv = [str(binary), "daemon", "serve", "--repo", str(repo), "--idle-timeout", "120"]
        with (output/"daemon.stdout").open("xb") as out, (output/"daemon.stderr").open("xb") as err:
            p = subprocess.Popen(argv, cwd=repo, env=env, stdout=out, stderr=err, start_new_session=True)
            try:
                deadline = time.monotonic()+60
                while not list(Path(tmp).glob("*.sock")):
                    if p.poll() is not None or time.monotonic()>deadline:
                        raise RuntimeError("daemon failed to become ready")
                    time.sleep(.02)
                path, = Path(tmp).glob("*.sock")
                socket_request(path, {"command":"ping"})
                row = dict(argv=argv, requests=requests, rss_ready=rss(p.pid), latencies_s=[], rss_samples=[])
                expected = None
                for i in range(requests):
                    start = time.monotonic()
                    result = socket_request(path, {"command":"symbol-batch", "queries":["main", "Request", "search", "run"], "limit":100})
                    row["latencies_s"].append(time.monotonic()-start)
                    fingerprint = digest(json.dumps(result,sort_keys=True).encode())
                    if expected is None:
                        expected=fingerprint
                        row["result_count"]=sum(len(x) for x in result["result"].values())
                        if not row["result_count"]:
                            raise RuntimeError("daemon workload returned no symbols")
                    if expected != fingerprint:
                        raise RuntimeError("daemon results changed between identical requests")
                    if i%10==0:
                        row["rss_samples"].append(rss(p.pid))
                row["response_sha256"]=expected
                row["rss_after_burst"]=rss(p.pid)
                time.sleep(2)
                row["rss_idle_2s"]=rss(p.pid)
                time.sleep(8)
                row["rss_idle_10s"]=rss(p.pid)
                socket_request(path,{"command":"shutdown"})
                if p.wait(timeout=10):
                    raise RuntimeError("daemon failed on shutdown")
                dump(output/"daemon.json",row)
                return row
            finally:
                if p.poll() is None:
                    os.killpg(p.pid,signal.SIGTERM)
                    try:
                        p.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        os.killpg(p.pid,signal.SIGKILL)
                        p.wait()


def main():
    ap=argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--build",type=Path,required=True)
    ap.add_argument("--source",action="append",required=True,help="label=source-only directory")
    ap.add_argument("--output",type=Path,required=True)
    ap.add_argument("--repetitions",type=int,default=5)
    ap.add_argument("--stage",choices=["index","product","daemon","concurrent"],default="index")
    ap.add_argument("--requests",type=int,default=100)
    args=ap.parse_args()
    if args.repetitions<1 or args.requests<1:
        ap.error("positive repetitions and requests required")
    build=json.loads(args.build.read_text())
    for arm in ARMS:
        for b in build["arms"][arm]["binaries"].values():
            if digest(Path(b["path"]).read_bytes()) != b["sha256"]:
                raise RuntimeError("binary provenance mismatch")
    sources={}
    for s in args.source:
        label,path=s.split("=",1); path=Path(path).resolve()
        if not label.replace("-","").isalnum() or label in sources:
            ap.error("invalid or duplicate source label")
        if (path/"packages/aethyme").exists() or (path/".aethyme").exists():
            ap.error("requires source-only Playground inputs; refusing Aethyme or enrolled repository")
        sources[label]=path
    args.output=args.output.resolve(); args.output.mkdir(parents=True,exist_ok=False)
    run=Runner(args.output)
    report=dict(platform=platform.platform(), build_sha256=digest(args.build.read_bytes()),
                runner_sha256=digest(Path(__file__).read_bytes()), stage=args.stage, repetitions=args.repetitions,
                rayon_threads=4, source_hashes={k:tree_hash(v) for k,v in sources.items()}, samples=[])
    dump(args.output/"report.json",report)
    with tempfile.TemporaryDirectory(prefix="allocator-playground-") as tmp:
        tmp=Path(tmp)
        for label,source in sources.items():
            reference={}
            for rep in range(args.repetitions):
                order=ARMS[rep%3:]+ARMS[:rep%3]
                for arm in order:
                    print(f"{args.stage}: {label} repetition {rep+1} {arm}",flush=True)
                    case=tmp/f"{label}-{rep}-{arm}"; case.mkdir()
                    repo=case/label; home=case/"home"; home.mkdir()
                    shutil.copytree(source,repo,ignore=shutil.ignore_patterns(".git","__pycache__"))
                    env=environment(home)
                    run.git(repo,env,"init","-q","-b","main")
                    run.git(repo,env,"add","-A")
                    run.git(repo,env,"commit","-qm","chore(benchmark): seed Playground")
                    seed=run.git(repo,env,"rev-parse","HEAD")
                    binaries=build["arms"][arm]["binaries"]
                    binary=lambda name: binaries[name]["path"]
                    item=dict(source=label,arm=arm,repetition=rep+1,order=order,seed_commit=seed,metrics={})
                    def measured(name,argv):
                        row,text=run.run(f"{label}-{rep}-{arm}-{name}",argv,repo,env)
                        item["metrics"][name]=row
                        return json.loads(text)
                    if args.stage=="concurrent":
                        workers=[]
                        for number in range(2):
                            worker_repo=case/f"worker-{number}"/label
                            worker_home=case/f"worker-home-{number}"
                            worker_home.mkdir()
                            shutil.copytree(repo,worker_repo)
                            worker_output=args.output/f"{label}-{rep}-{arm}-worker-{number}"
                            worker_output.mkdir()
                            workers.append((worker_repo,worker_home,worker_output))
                        def index_worker(worker):
                            worker_repo,worker_home,worker_output=worker
                            runner=Runner(worker_output)
                            row,text=runner.run("index-link",[binary("aethyme-graph-index"),"--repo-root",worker_repo,"--repo-name",label,"--engine-version","0.8.59","--json"],worker_repo,environment(worker_home))
                            return dict(metric=row,semantic=semantic(json.loads(text)),graph_root=str(worker_repo/".aethyme/graph"))
                        started=time.monotonic()
                        with ThreadPoolExecutor(max_workers=2) as pool:
                            results=list(pool.map(index_worker,workers))
                        item["concurrent_wall_s"]=time.monotonic()-started
                        item["workers"]=results
                        item["sum_process_peak_rss_bytes"]=sum(r["metric"]["peak_rss_bytes"] for r in results)
                        # Sum of process high-water marks is not simultaneous aggregate RSS.
                        for result in results:
                            result["graph_sha256"]=tree_hash(Path(result.pop("graph_root")))
                            if result["semantic"]["total_nodes"]<1:
                                raise RuntimeError("empty concurrent workload")
                            for key in ["semantic","graph_sha256"]:
                                if key in reference and reference[key]!=result[key]:
                                    raise RuntimeError(f"concurrent {key} mismatch")
                                reference[key]=result[key]
                    elif args.stage=="index":
                        result=measured("index",[binary("aethyme-graph-index"),"--repo-root",repo,"--repo-name",label,"--engine-version","0.8.59","--skip-link","--json"])
                        if result["total_files"]<1 or result["total_nodes"]<1:
                            raise RuntimeError("empty index workload")
                        item["index_semantic"]=semantic(result)
                        item["index_graph_sha256"]=tree_hash(repo/".aethyme/graph")
                        result=measured("link",[binary("aethyme-graph-link"),"--repo-root",repo,"--json"])
                        item["link_semantic"]=semantic(result)
                        item["linked_graph_sha256"]=tree_hash(repo/".aethyme/graph")
                        for key in ["index_semantic","link_semantic","index_graph_sha256","linked_graph_sha256"]:
                            if key in reference and reference[key]!=item[key]:
                                dump(args.output/"mismatch.json",dict(key=key,expected=reference[key],actual=item[key]))
                                raise RuntimeError(f"semantic mismatch: {label} {key}")
                            reference[key]=item[key]
                    else:
                        cli=binary("aethyme")
                        run.run("deploy",[cli,"deploy","--repo",repo,"--with-graph","--graph-repository",f"playground/{label}"],repo,env)
                        run.git(repo,env,"add","-A")
                        run.git(repo,env,"commit","-qm","chore(benchmark): enroll Playground")
                        _,text=run.run("plan",[cli,"graph","refresh","plan","--repo",repo,"--json"],repo,env)
                        plan=json.loads(text)
                        run.run("refresh",[cli,"graph","refresh","execute","--repo",repo,"--confirm",plan["plan_sha256"],"--json"],repo,env)
                        run.git(repo,env,"add","-A")
                        run.git(repo,env,"commit","--allow-empty","-qm","chore(benchmark): commit graph")
                        measured("materialize",[cli,"graph","materialize","--repo",repo,"--json"])
                        if args.stage=="product":
                            argv=[cli,"explore","--repo",repo,"--request","locate the primary application entrypoint","--format","answer-json","--show-observability","--depth","0"]
                            for name in ["explore_first","explore_repeat"]:
                                result=measured(name,argv)
                                # Keep complete answers; compare path-independent content separately.
                                selected={k: result[k] for k in ["schema_version", "mode", "intent", "status", "answer", "navigation_hints", "excluded", "ambiguous", "subsystems", "evidence", "confidence", "safe_to_use_as_answer", "safe_to_use_as_navigation", "trust_policy", "degraded_reasons", "verification_steps", "truncated"] if k in result}
                                if not selected.get("answer") and not selected.get("navigation_hints"):
                                    raise RuntimeError("Explore produced no answer or navigation hints")
                                text=json.dumps(selected,sort_keys=True).replace(str(repo),"<PLAYGROUND>").replace(str(home),"<HOME>")
                                item[name+"_semantic_sha256"]=digest(text.encode())
                                if name in reference and reference[name]!=item[name+"_semantic_sha256"]:
                                    dump(args.output/"mismatch.json", dict(name=name, actual=selected, expected_sha256=reference[name]))
                                    raise RuntimeError("Explore semantic mismatch")
                                reference[name]=item[name+"_semantic_sha256"]
                            probe=repo/"allocator-probe.md"; probe.write_text("# Allocator benchmark probe\n")
                            run.git(repo,env,"add",probe.name)
                            run.git(repo,env,"commit","-qm","test(benchmark): add one-file probe")
                            plan=measured("incremental_plan",[cli,"graph","refresh","plan","--repo",repo,"--json"])
                            result=measured("incremental_execute",[cli,"graph","refresh","execute","--repo",repo,"--confirm",plan["plan_sha256"],"--json"])
                            item["incremental_semantic"]=semantic(result)
                            item["incremental_counts"]=result["performance"]["counts"]
                            if "incremental_counts" in reference and reference["incremental_counts"]!=item["incremental_counts"]:
                                raise RuntimeError("incremental counts mismatch")
                            reference["incremental_counts"]=item["incremental_counts"]
                        else:
                            dest=args.output/f"{label}-{rep}-{arm}-daemon";dest.mkdir()
                            item["daemon"]=daemon_probe(binary("aethyme-engine-cli"),repo,env,dest,args.requests)
                            key=item["daemon"]["response_sha256"]
                            if "daemon" in reference and reference["daemon"]!=key:
                                raise RuntimeError("daemon results differ across allocators")
                            reference["daemon"]=key
                    report["samples"].append(item)
                    dump(args.output/"report.json",report)
                    shutil.rmtree(case)
    report["complete"]=True
    dump(args.output/"report.json",report)


if __name__=="__main__":
    main()
