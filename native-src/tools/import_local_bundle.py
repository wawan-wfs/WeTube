"""Import two trusted GitHub bootstrap tarballs, only after independent SHA256 verification."""
import argparse
import hashlib
import pathlib
import tarfile
import tempfile
import os

def extract(archive, digest, target, prefix):
    with archive.open("rb") as f:
        actual=hashlib.file_digest(f,"sha256").hexdigest()
    if actual.lower()!=digest.lower():
        raise ValueError("Archive SHA256 mismatch: "+archive.name)
    with tarfile.open(archive,"r:gz") as t:
        members=t.getmembers()
        total=0
        if len(members)>250000:
            raise ValueError("Too many bundle entries")
        seen=set()
        for m in members:
            p=pathlib.PurePosixPath(m.name)
            if p.is_absolute() or not p.parts or p.parts[0]!=prefix or ".." in p.parts or "\\" in m.name:
                raise ValueError("Unsafe archive path")
            if not (m.isfile() or m.isdir()) or p in seen:
                raise ValueError("Unsupported or duplicate bundle member")
            seen.add(p)
            total+=m.size
            if total>4*1024**3:
                raise ValueError("Bundle exceeds extraction budget")
        t.extractall(target,members=members,filter="data")

if __name__=="__main__":
    p=argparse.ArgumentParser()
    p.add_argument("--toolchain",type=pathlib.Path,required=True)
    p.add_argument("--toolchain-sha256",required=True)
    p.add_argument("--deps",type=pathlib.Path,required=True)
    p.add_argument("--deps-sha256",required=True)
    p.add_argument("--dest",type=pathlib.Path,required=True)
    a=p.parse_args()
    dest=a.dest.resolve()
    if dest.exists():
        raise ValueError("Destination already exists")
    dest.parent.mkdir(parents=True,exist_ok=True)
    with tempfile.TemporaryDirectory(dir=dest.parent) as tmp:
        stage=pathlib.Path(tmp)/"bundle"
        stage.mkdir()
        extract(a.toolchain,a.toolchain_sha256,stage,"toolchain")
        extract(a.deps,a.deps_sha256,stage,"deps")
        os.rename(stage,dest)
    print("Imported verified bundle:",dest)
