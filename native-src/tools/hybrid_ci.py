"""Local-first CI: run actual SQL literals and generated media locally.
Portable Rust runs only with a verified, matching offline dependency bundle.
Never calls a provider or declares native Windows acceptance.
"""
import argparse
import hashlib
import json
import os
import pathlib
import platform
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time

def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def check(ok, reason):
    if not ok:
        raise RuntimeError(reason)

def sql_probe(native, root):
    schema = re.search(r'const TABLES: &str = "([\s\S]*?)";', (native/"src/store.rs").read_text())[1]
    text = (native/"src/wetube_journal.rs").read_text()
    sql = dict(re.findall(r'const (SQL_\w+):&str=r#"([\s\S]*?)"#;', text))
    check(len(sql) == 12, "Missing journal SQL statements")
    path = root/"journal.db"
    db = sqlite3.connect(path, isolation_level=None)
    db.executescript("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;" + schema)
    marker = "wetube_download_journal_v2"
    ids = [f"{i:032x}" for i in range(10000)]
    def reconcile(rows):
        db.executescript(sql["SQL_TEMP"])
        db.execute("BEGIN IMMEDIATE")
        try:
            db.executemany(sql["SQL_SEEN"], [(i,) for i, _ in rows])
            db.executemany(sql["SQL_UPSERT"], [(i, n, v) for n, (i, v) in enumerate(rows)])
            for key in ("SQL_CLEAN", "SQL_PRUNE"):
                db.execute(sql[key])
            db.execute(sql["SQL_MARK"], (marker,))
            db.execute(sql["SQL_LEGACY"])
            db.execute("COMMIT")
        except Exception:
            db.execute("ROLLBACK")
            raise
    checks = []
    db.execute("INSERT INTO kv VALUES('download_history','private legacy')")
    t = time.perf_counter()
    reconcile([(i, '{"status":"queued"}') for i in ids])
    elapsed = (time.perf_counter()-t)*1000
    check(db.execute(sql["SQL_STATS"]).fetchone() == (10000,190000,19), "journal stats")
    check(db.execute("SELECT COUNT(*) FROM kv WHERE key='download_history'").fetchone()[0]==0, "legacy retained")
    checks.append("migration and aggregate byte statistics")
    try:
        reconcile([(ids[0],"changed"),(ids[0],"duplicate")])
        raise RuntimeError("Duplicate accepted")
    except sqlite3.IntegrityError:
        pass
    check(db.execute("SELECT COUNT(*) FROM wetube_download_jobs").fetchone()[0]==10000, "rollback lost rows")
    checks.append("duplicate rollback")
    i = ids[0]
    db.executemany("INSERT INTO kv VALUES(?1,'intent')", [("wetube_publish_"+i,),("wetube_resume_"+i,)])
    db.execute("BEGIN IMMEDIATE")
    db.execute(sql["SQL_UPDATE"], (i,'{"status":"done"}'))
    db.execute(sql["SQL_CLEAR_TERMINAL"],("wetube_publish_"+i,"wetube_resume_"+i))
    db.execute("COMMIT")
    check(db.execute("SELECT COUNT(*) FROM kv WHERE key LIKE 'wetube_publish_%' OR key LIKE 'wetube_resume_%'").fetchone()[0]==0,"terminal cleanup")
    checks.append("atomic terminal metadata cleanup")
    db.execute(sql["SQL_UPDATE"], (i, "x"*65537))
    stats=db.execute(sql["SQL_STATS"]).fetchone()
    check(stats[2]==65537 and db.execute(sql["SQL_READ"],(65536,)).fetchone()[1] is None,"oversize not visible")
    checks.append("SQL-side oversized row detection")
    db.execute(sql["SQL_UPDATE"], (i, "done"))
    extra="f"*32
    db.execute(sql["SQL_INSERT"],(extra,"queued"))
    check(db.execute("SELECT ordinal FROM wetube_download_jobs WHERE job_id=?",(extra,)).fetchone()[0]==10000,"admission ordinal")
    checks.append("single-row admission")
    child="import sqlite3,sys,os;c=sqlite3.connect(sys.argv[1]);c.execute('BEGIN IMMEDIATE');c.execute(sys.argv[2],(sys.argv[3],'uncommitted'));os._exit(47)"
    proc=subprocess.run([sys.executable,"-c",child,str(path),sql["SQL_UPDATE"],i],timeout=15)
    check(proc.returncode==47 and db.execute("SELECT json FROM wetube_download_jobs WHERE job_id=?",(i,)).fetchone()[0]=="done","hard exit rollback")
    checks.append("hard-exit rollback")
    db.execute("INSERT INTO kv VALUES(?,'intent')",("wetube_resume_"+i,))
    reconcile([])
    check(db.execute(sql["SQL_STATS"]).fetchone()==(0,0,0),"prune failed")
    check(db.execute("SELECT COUNT(*) FROM kv WHERE key=?",(marker,)).fetchone()[0]==1,"marker missing")
    check(db.execute("SELECT COUNT(*) FROM kv WHERE key LIKE 'wetube_resume_%'").fetchone()[0]==0,"orphan recovery")
    check(db.execute(sql["SQL_UPDATE"],(i,"late")).rowcount==0,"late worker resurrected row")
    db.close()
    checks.append("prune, migration marker and late worker")
    return {"status":"PASS","checks":checks,"rows":10000,"migration_ms":round(elapsed,2),
            "sqlite":sqlite3.sqlite_version,"scope":"production SQL literals, not Rust control-flow"}

def media_probe(root):
    if not shutil.which("ffmpeg"):
        return {"status":"NOT TESTED","reason":"FFmpeg missing"}
    fixture=root/"motion.mkv"
    subprocess.run(["ffmpeg","-v","error","-f","lavfi","-i","testsrc2=size=160x90:rate=10",
                    "-t","4","-c:v","ffv1","-y",str(fixture)],check=True,timeout=30)
    result=subprocess.check_output(["ffmpeg","-v","error","-i",str(fixture),"-f","framemd5","-"],text=True,timeout=30)
    frames=[s.split(",")[-1].strip() for s in result.splitlines() if s and not s.startswith("#")]
    seek=subprocess.check_output(["ffmpeg","-v","error","-ss","2","-i",str(fixture),"-frames:v","1","-f","framemd5","-"],text=True,timeout=30)
    seek_hash=next(s.split(",")[-1].strip() for s in seek.splitlines() if s and not s.startswith("#"))
    check(len(frames)==40 and len(set(frames))>1 and seek_hash==frames[20],"decode/seek fixture")
    return {"status":"PASS","frames":len(frames),"unique_frames":len(set(frames)),
            "seek_frame":20,"fixture_sha256":sha(fixture),"scope":"FFmpeg local decode, not libmpv/GPUI"}

def rust_probe(native, root, bundle):
    if bundle is None:
        return {"status":"NOT TESTED","reason":"Verified portable toolchain/dependency bundle not supplied"}
    from portable_core import prepare,verify_lock
    meta=json.loads((bundle/"deps/bundle.json").read_text())
    check(meta["native_lock_sha256"]==sha(native/"Cargo.lock"),"Bundle is for a different native lock")
    check(platform.machine() in ("x86_64","AMD64") and sys.platform.startswith("linux"),"Linux x64 required")
    core=root/"portable"
    prepare(native,core)
    shutil.copyfile(bundle/"deps/Cargo.lock",core/"Cargo.lock")
    verify_lock(native/"Cargo.lock",core/"Cargo.lock")
    (core/".cargo").mkdir()
    (core/".cargo/config.toml").write_text('[source.crates-io]\nreplace-with="vendored-sources"\n[source.vendored-sources]\ndirectory='+json.dumps(str((bundle/"deps/vendor").resolve()))+"\n[net]\noffline=true\n")
    bin_dir=(bundle/"toolchain/bin").resolve()
    env=dict(os.environ,PATH=str(bin_dir)+os.pathsep+os.environ.get("PATH",""),RUSTC=str(bin_dir/"rustc"),
             CARGO_HOME=str(root/"cargo-home"),CARGO_NET_OFFLINE="true")
    version=subprocess.check_output([str(bin_dir/"rustc"),"-V"],text=True,env=env).strip()
    check(version.split()[1]==meta["rust_version"],"Toolchain version mismatch")
    result=subprocess.run([str(bin_dir/"cargo"),"test","--offline","--locked"],cwd=core,env=env,text=True,
                          stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=1800)
    return {"status":"PASS" if result.returncode==0 else "FAIL","rustc":version,
            "exit_code":result.returncode,"log":result.stdout[-100000:],"scope":"actual portable production modules with explicit app adapters"}

def main():
    p=argparse.ArgumentParser()
    p.add_argument("--source",type=pathlib.Path,default=pathlib.Path(__file__).resolve().parents[1])
    p.add_argument("--report",type=pathlib.Path,required=True)
    p.add_argument("--bundle",type=pathlib.Path)
    p.add_argument("--require-rust",action="store_true")
    a=p.parse_args()
    result={"schema":1,"native_lock_sha256":sha(a.source/"Cargo.lock"),"platform":platform.platform()}
    with tempfile.TemporaryDirectory(prefix="wetube-local-ci-") as d:
        root=pathlib.Path(d)
        for name,fn in [("sqlite",lambda:sql_probe(a.source,root)),("media",lambda:media_probe(root)),
                        ("portable_rust",lambda:rust_probe(a.source,root,a.bundle))]:
            try:
                result[name]=fn()
            except Exception as e:
                result[name]={"status":"FAIL","reason":str(e)}
    result["windows_acceptance"]={"status":"NOT TESTED","reason":"Use Windows mandatory CI"}
    a.report.parent.mkdir(parents=True,exist_ok=True)
    a.report.write_text(json.dumps(result,indent=2)+"\n")
    print(json.dumps({k:v.get("status") for k,v in result.items() if isinstance(v,dict)},indent=2))
    if any(isinstance(v,dict) and v.get("status")=="FAIL" for v in result.values()):
        return 1
    if a.require_rust and result["portable_rust"]["status"]!="PASS":
        return 2
    return 0

if __name__=="__main__":
    sys.exit(main())
