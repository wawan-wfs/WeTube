"""Import a trusted Debian SDK artifact without executing package maintainer scripts.
The expected outer SHA-256 must be obtained from the authorized GitHub artifact API.
The bootstrap workflow uses authenticated APT; this verifies transport and inventory.
"""
import argparse
import hashlib
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import zipfile

HEX = re.compile(r"[a-f0-9]{64}")
ARCHIVE = 'debian13-amd64-sdk.tar.gz'
CHECKSUM = 'debian13-amd64-sdk.sha256'

def digest(p):
    with p.open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()

def require(ok, why):
    if not ok:
        raise ValueError(why)

def import_sdk(artifact, expected, dest):
    require(HEX.fullmatch(expected) and digest(artifact) == expected, 'Outer artifact checksum mismatch')
    dest=dest.resolve()
    require(not dest.exists(), 'Destination must not exist')
    dest.parent.mkdir(parents=True,exist_ok=True)
    with tempfile.TemporaryDirectory(dir=dest.parent,prefix='wetube-sdk-') as td:
        root=Path(td)
        with zipfile.ZipFile(artifact) as z:
            names=z.namelist()
            require(len(names)==2 and set(names)=={ARCHIVE,CHECKSUM},'Unexpected ZIP entries')
            for name in names:
                size=z.getinfo(name).file_size
                require(0<size<=(1024**3 if name==ARCHIVE else 1024),'ZIP entry size limit')
                with z.open(name) as source,(root/name).open('xb') as output:
                    shutil.copyfileobj(source,output)
        sha,name=(root/CHECKSUM).read_text().strip().split()
        require(name==ARCHIVE and HEX.fullmatch(sha) and digest(root/ARCHIVE)==sha,'Inner checksum mismatch')
        debs=root/'debs';debs.mkdir()
        with tarfile.open(root/ARCHIVE,'r:gz') as t:
            entries=t.getmembers();names=[m.name for m in entries]
            require(len(entries)<=1000 and len(set(names))==len(names),'Duplicate or excessive SDK entries')
            require(sum(m.size for m in entries)<=2*1024**3,'SDK archive size limit')
            for m in entries:
                require(m.isfile() and m.name not in ('.','..') and '/' not in m.name and '\\' not in m.name and not m.name.startswith('-'),'Unsafe SDK archive member')
                require(m.name.endswith('.deb') or m.name in ('SHA256SUMS','inventory.tsv','os-release'),'Unexpected archive member')
            t.extractall(debs,members=entries,filter='data')
        manifest={}
        for line in (debs/'SHA256SUMS').read_text().splitlines():
            sha,name=line.split()
            require(HEX.fullmatch(sha) and name in names and name.endswith('.deb') and name not in manifest,'Invalid package digest entry')
            manifest[name]=sha
        inventory={}
        for line in (debs/'inventory.tsv').read_text().splitlines():
            name,package,version,arch=line.split('\t')
            require(name not in inventory and arch in ('amd64','all'),'Invalid SDK package inventory')
            inventory[name]=(package,version,arch)
        require(set(manifest)==set(inventory)=={n for n in names if n.endswith('.deb')},'Package inventory mismatch')
        require('VERSION_CODENAME=trixie' in (debs/'os-release').read_text(),'Expected Debian 13 SDK')
        for name,sha in manifest.items():
            p=debs/name;require(digest(p)==sha,'Package checksum mismatch: '+name)
            fields=tuple(subprocess.check_output(['dpkg-deb','-f',str(p),field],text=True,timeout=30).strip() for field in ('Package','Version','Architecture'))
            require(fields==inventory[name],'Package metadata mismatch: '+name)
        sysroot=root/'sysroot';sysroot.mkdir()
        for name in sorted(manifest):
            subprocess.run(['dpkg-deb','-x',str(debs/name),str(sysroot)],check=True,timeout=120)
        os.rename(sysroot,dest)
        print(f'Imported {len(manifest)} verified Debian packages to {dest}; no maintainer scripts executed.')
        print('PKG_CONFIG_SYSROOT_DIR='+str(dest))
        print('PKG_CONFIG_PATH='+str(dest/'usr/lib/x86_64-linux-gnu/pkgconfig')+':'+str(dest/'usr/share/pkgconfig'))

if __name__=='__main__':
    parser=argparse.ArgumentParser()
    parser.add_argument('--artifact',type=Path,required=True)
    parser.add_argument('--sha256',required=True)
    parser.add_argument('--dest',type=Path,required=True)
    args=parser.parse_args();import_sdk(args.artifact,args.sha256,args.dest)
