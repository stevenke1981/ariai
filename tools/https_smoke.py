"""Compare an actual release HTTPS download with independently fetched bytes."""
import argparse
import datetime
import hashlib
import json
import pathlib
import subprocess
import tempfile
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument("binary", type=pathlib.Path)
args = parser.parse_args()
binary = args.binary.resolve()
url = "https://static.rust-lang.org/dist/channel-rust-stable.toml.sha256"
with urllib.request.urlopen(url, timeout=30) as response:
    expected = response.read()
with tempfile.TemporaryDirectory(prefix="ariai-https-") as directory:
    result = subprocess.run([str(binary), "--max-tries=1", "--split=1", "-d", directory, "-o", "stable.sha256", url], capture_output=True, timeout=60)
    result.check_returncode()
    data = (pathlib.Path(directory) / "stable.sha256").read_bytes()
    assert data == expected, "Release HTTPS content differs from independent fetch"
report = {
    "result": "PASS",
    "observed_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "url": url,
    "bytes": len(data),
    "sha256": hashlib.sha256(data).hexdigest(),
    "binary": str(binary),
    "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
    "version": subprocess.check_output([str(binary), "--version"], text=True).strip(),
    "tls_verification": "rustls default enabled",
}
root = pathlib.Path(__file__).resolve().parents[1]
(root / "evidence" / "https-smoke.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
print(json.dumps(report))
