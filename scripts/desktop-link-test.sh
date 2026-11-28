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

<a href="https://example.com/landing?x=1" style="display:block;height:600px;font-size:40px;padding:30px">OPEN THIS LINK</a>
EML

# webbrowser crate 在 Linux 上遵循 $BROWSER：用假浏览器记录被打开的地址。
cat > "$WORK/browser.sh" <<SH
#!/usr/bin/env bash
echo "\$@" >> "$WORK/opened.txt"
SH
chmod +x "$WORK/browser.sh"

BROWSER="$WORK/browser.sh" "$BIN" --email "$WORK/link.eml" >"$WORK/app.log" 2>&1 &
PID=$!

# 等窗口出现（最多 60 秒），再给页面一些时间加载。
WID="$(timeout 60 xdotool search --sync --name '^Letterdesk$' | head -1 || true)"
if [ -z "$WID" ]; then
  echo "失败：60 秒内没有出现 Letterdesk 窗口" >&2
  tail -20 "$WORK/app.log" >&2 || true
  exit 1
fi
sleep 5

# 链接是一个很高的块，占满来信 iframe；在阅读区中部相对窗口点击，最多重试 5 次。
for attempt in 1 2 3 4 5; do
  xdotool mousemove --window "$WID" 600 330 click 1
  sleep 3
  if grep -qx "https://example.com/landing?x=1" "$WORK/opened.txt" 2>/dev/null; then
    echo "通过：链接已交给系统浏览器打开（第 $attempt 次点击）"
    exit 0
  fi
done

echo "失败：点击链接后没有在系统浏览器中打开" >&2
xdotool getwindowgeometry "$WID" >&2 || true
if [ -n "${ARTIFACT_DIR:-}" ] && command -v import >/dev/null; then
  mkdir -p "$ARTIFACT_DIR"
  import -window root "$ARTIFACT_DIR/desktop-link-test.png" || true
  cp "$WORK/app.log" "$ARTIFACT_DIR/desktop-link-test.log" || true
fi
exit 1
