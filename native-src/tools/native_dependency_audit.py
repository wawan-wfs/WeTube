"""Check a Cargo vendor inventory against the complete native lockfile.
No network, Cargo resolution or package execution is performed here.
"""
import argparse
import hashlib
import json
import pathlib
import sys
import tomllib


def audit(lock, vendor, verify_files=False):
    packages = tomllib.loads(lock.read_text(encoding="utf-8"))["package"]
    expected = {(p["name"], p["version"]): p["checksum"] for p in packages if p.get("source", "").startswith("registry+")}
    actual = set()
    errors = []
    for root in sorted(vendor.iterdir()):
        if not root.is_dir():
            continue
        try:
            if root.is_symlink():
                raise ValueError("symlink package directory")
            p = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))["package"]
            key = (p["name"], p["version"])
            manifest = json.loads((root / ".cargo-checksum.json").read_text(encoding="utf-8"))
            if key not in expected or manifest.get("package") != expected[key] or key in actual:
                raise ValueError("unexpected package, checksum mismatch or duplicate")
            if verify_files:
                for rel, digest in manifest["files"].items():
                    path = root / rel
                    if pathlib.PurePath(rel).is_absolute() or ".." in pathlib.PurePath(rel).parts or not path.resolve().is_relative_to(root.resolve()) or path.is_symlink() or not path.is_file():
                        raise ValueError("unsafe package file path")
                    with path.open("rb") as stream:
                        if hashlib.file_digest(stream, "sha256").hexdigest() != digest:
                            raise ValueError("file checksum mismatch: " + rel)
            actual.add(key)
        except Exception as e:
            errors.append({"directory": root.name, "reason": str(e)})
    missing = sorted(expected.keys() - actual)
    return {"lock_sha256": hashlib.sha256(lock.read_bytes()).hexdigest(), "registry_packages": len(expected),
            "verified_packages": len(actual), "missing_packages": len(missing),
            "missing": [{"name": n, "version": v} for n, v in missing], "errors": errors,
            "file_hashes_verified": verify_files, "status": "PASS" if not missing and not errors else "INCOMPLETE"}


if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("--lock", type=pathlib.Path, required=True)
    p.add_argument("--vendor", type=pathlib.Path, required=True)
    p.add_argument("--report", type=pathlib.Path, required=True)
    p.add_argument("--verify-files", action="store_true")
    p.add_argument("--require-full", action="store_true")
    a = p.parse_args()
    result = audit(a.lock, a.vendor, a.verify_files)
    a.report.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({k:v for k,v in result.items() if k not in ("missing", "errors")}))
    sys.exit(1 if result["errors"] or (a.require_full and result["status"] != "PASS") else 0)
