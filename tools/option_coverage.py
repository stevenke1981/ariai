"""Map the upstream option inventory to flags advertised by the built CLI."""
import argparse
import pathlib
import re
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument("binary", type=pathlib.Path)
args = parser.parse_args()
root = pathlib.Path(__file__).resolve().parents[1]
help_text = subprocess.check_output([str(args.binary.resolve()), "--help"], text=True, encoding="utf-8")
accepted = set(re.findall(r"--[a-z][a-z0-9-]*", help_text))
upstream = [line.strip() for line in (root / "docs" / "aria2-options.txt").read_text(encoding="utf-8").splitlines() if line.startswith("--")]
version = subprocess.check_output([str(args.binary.resolve()), "--version"], text=True).strip()
lines = [f"# {version} 選項受理對照\n", "由 tools/option_coverage.py 依本次執行檔 --help 與上游選項盤點產生。這只表示 CLI 有受理旗標，不表示語義完整相容或每個選項已實測；範圍和差異見 [功能矩陣](FEATURES.md)。\n", f"上游盤點 {len(upstream)} 項，其中 {len(set(upstream) & accepted)} 項出現在本次 CLI help。這不是功能完成百分比。\n", "| 上游選項 | ariai CLI |\n|---|---|\n"]
for option in upstream:
    lines.append(f"| `{option}` | {'已受理，語義見功能矩陣' if option in accepted else '待補'} |\n")
extensions = sorted(accepted - set(upstream))
lines.append("\nariai 額外旗標：" + "、".join(f"`{option}`" for option in extensions) + "。\n")
(root / "docs" / "OPTION-COVERAGE.md").write_text("\n".join(lines), encoding="utf-8")
print(f"{version}: {len(set(upstream) & accepted)}/{len(upstream)} upstream flag names accepted; semantic parity tracked separately")
