"""Package an already verified Windows release without installing it globally."""
import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import zipfile

root = pathlib.Path(__file__).resolve().parents[1]
target = pathlib.Path(os.environ.get("CARGO_TARGET_DIR", root / "target"))
executable = target / "release" / "ariai.exe"
if not executable.is_file():
    raise SystemExit("Build and verify the Windows release first.")
verification = json.loads((root / "evidence" / "verification.json").read_text(encoding="utf-8"))
if verification["result"] != "PASS" or verification["artifact"]["sha256"] != hashlib.sha256(executable.read_bytes()).hexdigest():
    raise SystemExit("The release executable must match passing acceptance evidence.")
for relative, expected in verification["source_sha256"].items():
    if hashlib.sha256((root / relative).read_bytes()).hexdigest() != expected:
        raise SystemExit(f"Source changed after acceptance: {relative}")
folder = root / "dist" / "ariai-windows-x64"
folder.mkdir(parents=True, exist_ok=True)
files = {
    executable: "ariai.exe",
    root / "README.md": "README.md",
    root / "LICENSE": "LICENSE",
    root / "PLAN.md": "PLAN.md",
    root / "PLAN-PHASE2.md": "PLAN-PHASE2.md",
    root / "SPEC.md": "SPEC.md",
    root / "docs" / "FEATURES.md": "docs/FEATURES.md",
    root / "docs" / "OPTION-COVERAGE.md": "docs/OPTION-COVERAGE.md",
    root / "docs" / "aria2-options.txt": "docs/aria2-options.txt",
    root / "docs" / "SOURCES.md": "docs/SOURCES.md",
    root / "examples" / "ariai.conf": "examples/ariai.conf",
    root / "ACCEPTANCE.md": "ACCEPTANCE.md",
    root / "evidence" / "verification.json": "evidence/verification.json",
    root / "evidence" / "router-verification.json": "evidence/router-verification.json",
    root / "evidence" / "agents.json": "evidence/agents.json",
    root / "evidence" / "integration.md": "evidence/integration.md",
    root / "evidence" / "https-smoke.json": "evidence/https-smoke.json",
}
manifest = {}
for source, relative in files.items():
    destination = folder / relative
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)
    manifest[relative] = hashlib.sha256(destination.read_bytes()).hexdigest()
metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--format-version=1"], cwd=root))
notices = ["Third-party crate license declarations and bundled license/notice texts.\nVersions are fixed by Cargo.lock. This includes development and platform dependencies.\n"]
for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
    if package["source"] is None:
        continue
    notices.append(f"\n{'=' * 72}\n{package['name']} {package['version']}\nLicense: {package.get('license') or 'see license file'}\nRepository: {package.get('repository') or ''}\n")
    package_dir = pathlib.Path(package["manifest_path"]).parent
    license_paths = {path for path in package_dir.iterdir() if path.is_file() and path.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE", "COPYRIGHT"))}
    if package.get("license_file"):
        license_paths.add(package_dir / package["license_file"])
    for path in sorted(license_paths):
        notices.append(f"\n--- {path.name} ---\n{path.read_text(encoding='utf-8', errors='replace')}\n")
notice_file = folder / "THIRD-PARTY-NOTICES.txt"
notice_file.write_text("".join(notices), encoding="utf-8")
manifest[notice_file.name] = hashlib.sha256(notice_file.read_bytes()).hexdigest()
(folder / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
archive = root / "dist" / "ariai-windows-x64.zip"
with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as output:
    for relative in [*manifest, "manifest.json"]:
        output.write(folder / relative, f"ariai-windows-x64/{relative}")
print(json.dumps({"archive": str(archive), "sha256": hashlib.sha256(archive.read_bytes()).hexdigest(), "executable_sha256": manifest["ariai.exe"]}))
