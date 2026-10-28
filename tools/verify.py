"""Independent acceptance entry point. Run from any directory with Python 3."""
import argparse
import datetime
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--target-dir", type=pathlib.Path, help="Cargo build output directory")
    args = parser.parse_args()
    if args.target_dir:
        os.environ["CARGO_TARGET_DIR"] = str(args.target_dir.resolve())
    evidence = ROOT / "evidence"
    evidence.mkdir(exist_ok=True)
    target = pathlib.Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    binary = target / "release" / ("ariai.exe" if os.name == "nt" else "ariai")
    commands = [
        ("format", ["cargo", "fmt", "--all", "--", "--check"]),
        ("tests", ["cargo", "test", "--locked"]),
        ("clippy", ["cargo", "clippy", "--locked", "--all-targets", "--", "-D", "warnings"]),
        ("release", ["cargo", "build", "--release", "--locked"]),
        ("version", [str(binary), "--version"]),
        ("cli-smoke", [sys.executable, "-X", "utf8", str(ROOT / "tools" / "smoke.py"), str(binary)]),
    ]
    report = {"observed_at": datetime.datetime.now(datetime.timezone.utc).isoformat(), "platform": sys.platform, "checks": [], "result": "PASS"}
    source_files = [ROOT / "Cargo.toml", ROOT / "Cargo.lock", *sorted((ROOT / "src").rglob("*.rs")), *sorted((ROOT / "tests").rglob("*.rs"))]
    report["source_sha256"] = {str(path.relative_to(ROOT)): hashlib.sha256(path.read_bytes()).hexdigest() for path in source_files}
    for name, argv in commands:
        start = time.monotonic()
        print(f"RUN {name}", flush=True)
        result = subprocess.run(argv, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=1200)
        log = evidence / f"{name}.log"
        log.write_bytes(result.stdout)
        report["checks"].append({"name": name, "argv": argv, "exit_code": result.returncode, "result": "PASS" if result.returncode == 0 else "FAIL", "seconds": round(time.monotonic() - start, 2), "output": str(log.relative_to(ROOT)), "sha256": hashlib.sha256(result.stdout).hexdigest()})
        print(result.stdout.decode("utf-8", errors="replace"), flush=True)
        if result.returncode:
            report["result"] = "FAIL"
            break
    if binary.exists():
        report["artifact"] = {"path": str(binary), "sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}
    (evidence / "verification.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return 0 if report["result"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())

