"""Verify the exact committed upstream and candidate source bytes, then export an atomic source ZIP.
Run from repository root. No network and no execution of source code.
"""
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import zipfile

UPSTREAM = "9904212b3323512eb009d77322ecf999dcdc92fc"
REV = "f377d0685458ff542f22e7cc98c110a4275b19de"
REQUIRED = {"Cargo.toml", "Cargo.lock", "LICENSE", "src/main.rs", "src/wetube_session.rs",
            "src/wetube_migration.rs", "src/wetube_import_schema.sql", "src/ui/app.rs",
            "src/wetube_links.rs", "src/wetube_queue.rs", "src/wetube_runtime.rs", "installer/runtime-notice.txt",
            "src/player/mpv.rs", "src/store.rs", "vendor/gpui-pre-windows/Cargo.toml",
            "installer/wetube.iss", "WETUBE_BUILD.json", "assets/streamtube.ico"}
MAX_BYTES = 100_000_000

def git(*args):
    return subprocess.check_output(["git", *args])

def tree(prefix):
    rows = {}
    folded = set()
    for item in git("ls-tree", "-r", "-z", "HEAD:" + prefix).split(b"\0"):
        if not item:
            continue
        meta, path = item.split(b"\t", 1)
        mode, kind, sha = meta.decode("ascii").split()
        name = path.decode("utf-8")
        if mode != "100644" or kind != "blob":
            raise ValueError("Unsupported entry: " + name)
        if "\\" in name or ":" in name or any(p in ("", ".", "..") or p.lower() == ".git" for p in name.split("/")):
            raise ValueError("Unsafe path: " + name)
        if name.casefold() in folded:
            raise ValueError("Case collision: " + name)
        folded.add(name.casefold())
        rows[name] = sha
    return rows

def blob_hash(data):
    return hashlib.sha1(b"blob " + str(len(data)).encode("ascii") + b"\0" + data).hexdigest()

def read_verified(prefix, rows):
    root = pathlib.Path(prefix)
    result = {}
    total = 0
    actual = {p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file()}
    if actual != set(rows):
        raise ValueError("Unexpected or missing worktree files under " + prefix)
    for name, sha in sorted(rows.items()):
        p = root / name
        if p.is_symlink() or not p.is_file() or any(a.is_symlink() for a in p.parents):
            raise ValueError("Unsafe file: " + str(p))
        total += p.stat().st_size
        if total > MAX_BYTES:
            raise ValueError("Source too large")
        data = p.read_bytes()
        if blob_hash(data) != sha:
            raise ValueError("Worktree bytes differ from committed blob: " + str(p))
        result[name] = data
    return result

def mutable(name):
    return name in {"Cargo.toml", "Cargo.lock", "README.md", "assets/logo.svg", "assets/logo.png", "assets/streamtube.ico", "installer/wetube.iss"} or (name.startswith("src/") and name.endswith(".rs")) or name.startswith("assets/accent/")

def audit(up, native):
    if len(up) != 120 or not REQUIRED <= native.keys():
        raise ValueError("Missing required source")
    if any(n.startswith(".github/workflows/") for n in native):
        raise ValueError("Nested native workflows forbidden in this checkpoint")
    for name, sha in up.items():
        if name.startswith(".github/workflows/"):
            continue
        if name not in native or (native[name] != sha and not mutable(name)):
            raise ValueError("Missing or unauthorized upstream change: " + name)
    for name in ("src/wetube_migration.rs", "src/wetube_session.rs", "src/wetube_import_schema.sql", "src/wetube_links.rs", "src/wetube_queue.rs"):
        expected = {"src/wetube_migration.rs":"081f2e759c07ee1cd342769cb7a13da2c9335183",
                    "src/wetube_session.rs":"9497835e0a5c1e7dae8ba19afc7c52c39684cd28",
                    "src/wetube_links.rs":"ea285f009bc2b27f6c971afdb1fb52dfd756b4cd",
                    "src/wetube_queue.rs":"1e9344db18e540c933e997fa87651f263b4a47a7",
                    "src/wetube_import_schema.sql":"58dbd39626f1296829b7a197fc07d929e616e37d"}[name]
        if native[name] != expected:
            raise ValueError("Baseline module bytes changed: " + name)

def main(out):
    if git("rev-parse", "HEAD:upstream-original").decode().strip() != UPSTREAM:
        raise ValueError("Wrong upstream tree")
    up, native = tree("upstream-original"), tree("native-src")
    audit(up, native)
    read_verified("upstream-original", up)
    data = read_verified("native-src", native)
    ident = json.loads(data["WETUBE_BUILD.json"])
    if ident != {"product":"WeTube Desktop","publisher":"WFS Project","version":"0.8.0","commit":REV,"tree":UPSTREAM,"native":True}:
        raise ValueError("Wrong build identity")
    seal = {"schema":1, "upstream_commit":REV, "upstream_tree":UPSTREAM,
            "checkpoint_commit":git("rev-parse","HEAD").decode().strip(),
            "native_tree":git("rev-parse","HEAD:native-src").decode().strip(),
            "files":{n:{"git_blob":native[n],"sha256":hashlib.sha256(b).hexdigest(),"size":len(b)} for n,b in data.items()}}
    if "WETUBE_SOURCE_INTEGRITY.json" in data:
        raise ValueError("Seal path already present")
    data["WETUBE_SOURCE_INTEGRITY.json"] = (json.dumps(seal,sort_keys=True,indent=2)+"\n").encode()
    out = pathlib.Path(out).resolve()
    if out.exists():
        raise ValueError("Output exists")
    out.parent.mkdir(parents=True,exist_ok=True)
    with tempfile.TemporaryDirectory(dir=out.parent) as d:
        stage=pathlib.Path(d)/"source.zip"
        with zipfile.ZipFile(stage,"x",zipfile.ZIP_DEFLATED) as z:
            for n,b in sorted(data.items()):
                z.writestr("WeTube-Desktop-Native/"+n,b)
        with zipfile.ZipFile(stage) as z:
            if z.testzip() or set(z.namelist()) != {"WeTube-Desktop-Native/"+n for n in data}:
                raise ValueError("ZIP integrity failure")
            for n,b in data.items():
                if z.read("WeTube-Desktop-Native/"+n)!=b:
                    raise ValueError("ZIP content mismatch: "+n)
        os.link(stage,out)
    print("Verified original files:",len(up))
    print("Verified native tracked files:",len(native))
    print("Native tree:",seal["native_tree"])
    print("Source ZIP SHA256:",hashlib.sha256(out.read_bytes()).hexdigest())

if __name__ == "__main__":
    main(sys.argv[1])
