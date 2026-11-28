#!/usr/bin/env bash
# 桌面原型：在来信 iframe 中真实点击一个链接，检查它在系统浏览器中打开、而不是在应用内打开。
# 需要 Linux、Xvfb 和 xdotool。用法：
#   xvfb-run -a scripts/desktop-link-test.sh [letterdesk-desktop 可执行文件]
set -euo pipefail

BIN="${1:-target/debug/letterdesk-desktop}"
WORK="$(mktemp -d)"
trap 'kill "${PID:-0}" 2>/dev/null || true; rm -rf "$WORK"' EXIT

cat > "$WORK/link.eml" <<'EML'
From: Maya Patel <maya@example.com>
To: Lin Chen <lin@example.com>
Subject: Link test
Content-Type: text/html; charset=utf-8

<a href="https://example.com/landing?x=1" style="display:block;font-size:40px;padding:30px">OPEN THIS LINK</a>
EML

# webbrowser crate 在 Linux 上遵循 $BROWSER：用假浏览器记录被打开的地址。
cat > "$WORK/browser.sh" <<SH
#!/usr/bin/env bash
echo "\$@" >> "$WORK/opened.txt"
SH
chmod +x "$WORK/browser.sh"

BROWSER="$WORK/browser.sh" "$BIN" --email "$WORK/link.eml" >/dev/null 2>&1 &
PID=$!
sleep 8
xdotool mousemove 300 200 click 1
sleep 3

if grep -qx "https://example.com/landing?x=1" "$WORK/opened.txt" 2>/dev/null; then
  echo "通过：链接已交给系统浏览器打开"
else
  echo "失败：点击链接后没有在系统浏览器中打开" >&2
  exit 1
fi
