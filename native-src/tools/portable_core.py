"""Stage production core modules outside the GPUI build, with explicit adapters.
No network in this script. The native Cargo.lock is the dependency authority.
"""
import argparse
import hashlib
import pathlib
import shutil
import tomllib

MODULES = ("wetube_resume", "wetube_profile", "wetube_process", "wetube_journal",
           "wetube_session", "wetube_queue", "wetube_recovery", "wetube_links", "wetube_publish", "wetube_transfer")
DEPS = {"anyhow": "", "serde": 'features = ["derive"]', "serde_json": "",
        "rand": "", "sha2": "", "rusqlite": 'features = ["bundled"]',
        "ureq": 'features = ["json", "socks-proxy"]'}
ADAPTERS = """
// Portable scope: production algorithms, loopback HTTP, journal, profile and
// publication fixtures. No GPUI, Windows Job Object, or application profile.
pub mod api {
    pub mod types;
    pub mod http;
    pub mod innertube {
        pub fn default_thumb(id:&str)->String { format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg") }
    }
}
pub mod store {
    use std::{collections::BTreeMap,sync::{LazyLock,Mutex}};
    static KV:LazyLock<Mutex<BTreeMap<String,String>>>=LazyLock::new(||Mutex::new(BTreeMap::new()));
    pub struct Settings {pub wetube_restore_tabs:bool,pub wetube_recover_playback:bool}
    pub fn settings_snapshot()->Settings{Settings{wetube_restore_tabs:false,wetube_recover_playback:false}}
    pub fn kv_get(key:&str)->Option<String>{KV.lock().unwrap().get(key).cloned()}
    pub fn wetube_kv_read(key:&str)->anyhow::Result<Option<String>>{Ok(kv_get(key))}
    pub fn wetube_kv_set(key:&str,value:&str)->bool{KV.lock().unwrap().insert(key.into(),value.into());true}
    pub fn ip_family()->ureq::config::IpFamily{ureq::config::IpFamily::Any}
    pub fn proxy_setting()->Option<String>{None}
}
pub mod downloads {
    pub struct Job{pub durable_id:String}
    pub fn valid_durable_id(id:&str)->bool{id.len()==32&&id.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b))}
    pub fn commit_download(from:&std::path::Path,to:&std::path::Path)->std::io::Result<()>{
        std::fs::hard_link(from,to)
    }
}
"""

def lock_data(path):
    return tomllib.loads(path.read_text(encoding="utf-8"))

def verify_lock(native, portable):
    base = lock_data(native)
    allowed = {(p["name"], p["version"], p.get("source"), p.get("checksum")) for p in base["package"]}
    for p in lock_data(portable)["package"]:
        identity = (p["name"], p["version"], p.get("source"), p.get("checksum"))
        if identity not in allowed:
            raise ValueError("Portable dependency drift: " + p["name"] + " " + p["version"])
        if p["name"] != "wetube" and not p.get("checksum"):
            raise ValueError("Portable dependency lacks registry checksum: " + p["name"])

def prepare(native, dest):
    native = native.resolve()
    dest = dest.resolve()
    if dest.exists() and any(dest.iterdir()):
        raise ValueError("Portable destination must be empty")
    packages = lock_data(native / "Cargo.lock")["package"]
    root = next(p for p in packages if p["name"] == "wetube" and "source" not in p)
    lines = ['[package]', 'name = "wetube"', 'version = "' + root["version"] + '"',
             'edition = "2024"', 'publish = false', '[dependencies]']
    for name, extra in DEPS.items():
        matches = [p for p in packages if p["name"] == name and any(
            d == name or d == name + " " + p["version"] for d in root["dependencies"])]
        if len(matches) != 1:
            raise ValueError("Ambiguous pinned dependency " + name)
        lines.append(name + ' = { version = "=' + matches[0]["version"] + '"' +
                     (", " + extra if extra else "") + " }")
    (dest / "src/api").mkdir(parents=True)
    (dest / "Cargo.toml").write_text("\n".join(lines) + "\n", encoding="utf-8")
    shutil.copyfile(native / "Cargo.lock", dest / "Cargo.lock")
    for module in MODULES:
        shutil.copyfile(native / "src" / (module + ".rs"), dest / "src" / (module + ".rs"))
    for module in ("types", "http"):
        shutil.copyfile(native / "src/api" / (module + ".rs"), dest / "src/api" / (module + ".rs"))
    (dest / "src/lib.rs").write_text(ADAPTERS + "\n" + "\n".join("pub mod " + m + ";" for m in MODULES) + "\n",
                                    encoding="utf-8")
    return hashlib.sha256((native / "Cargo.lock").read_bytes()).hexdigest()

if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("native", type=pathlib.Path)
    p.add_argument("dest", type=pathlib.Path)
    p.add_argument("--verify-lock", action="store_true")
    a = p.parse_args()
    if a.verify_lock:
        verify_lock(a.native / "Cargo.lock", a.dest / "Cargo.lock")
    else:
        print("Native lock SHA256:", prepare(a.native, a.dest))
