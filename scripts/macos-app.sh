#!/bin/zsh
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
if [[ -f "$HOME/.cargo/env" ]]; then
  source "$HOME/.cargo/env"
fi
mkdir -p "$root/dist"
cargo build --release --message-format=json >"$root/dist/build.json"
bin="$(python3 -c '
import json, sys
path = ""
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    line = line.strip()
    if not line.startswith("{"):
        continue
    try:
        msg = json.loads(line)
    except json.JSONDecodeError:
        continue
    if (
        msg.get("reason") == "compiler-artifact"
        and msg.get("target", {}).get("name") == "audiolink"
        and msg.get("executable")
    ):
        path = msg["executable"]
if not path:
    raise SystemExit("没有找到编译出的 audiolink")
print(path)
' "$root/dist/build.json")"
rm -f "$root/dist/build.json"
app="$root/dist/音频互联.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp "$bin" "$app/Contents/MacOS/audiolink"
cp "$root/packaging/macos/Info.plist" "$app/Contents/Info.plist"
chmod +x "$app/Contents/MacOS/audiolink"
codesign --force --sign - "$app"
echo "已生成 $app"
