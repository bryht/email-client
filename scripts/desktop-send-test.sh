#!/usr/bin/env bash
# 桌面原型端到端发信测试（Gmail 模拟服务）：同步 → 在写信框输入 → 点“发送” →
# 用 IMAP 检查“已发送”里出现了带正确线程信息和收件人的回信。
# 需要 Linux、Xvfb、xdotool、curl，并已构建 letterdesk-mock、letterdesk、letterdesk-desktop。
#   xvfb-run -a -s "-screen 0 1280x820x24" scripts/desktop-send-test.sh
set -euo pipefail

BIN="${BIN:-target/debug}"
WORK="$(mktemp -d)"
PORTS=(21143 21025 28901)
cleanup() {
  kill "${APP:-0}" "${MOCK:-0}" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

export LETTERDESK_GMAIL_APP_PASSWORD="abcd efgh ijkl mnop"
export LETTERDESK_CONFIG_DIR="$WORK/config"
IMAP="imap://127.0.0.1:${PORTS[0]}/%5BGmail%5D%2FSent%20Mail"
AUTH="lin@example.com:abcdefghijklmnop"

"$BIN/letterdesk-mock" --mailbox samples --me lin@example.com \
  --imap-port "${PORTS[0]}" --smtp-port "${PORTS[1]}" --graph-port "${PORTS[2]}" >"$WORK/mock.log" 2>&1 &
MOCK=$!
sleep 1

"$BIN/letterdesk" sync gmail --mock "127.0.0.1:${PORTS[0]}:${PORTS[1]}" --user lin@example.com --all --out "$WORK/mail" 2>&1 | grep 同步完成
echo "{\"kind\":\"gmail\",\"address\":\"lin@example.com\",\"mock\":\"127.0.0.1:${PORTS[0]}:${PORTS[1]}\",\"days\":0}" >"$WORK/account.json"

before="$(curl -s --noproxy '*' "$IMAP" -u "$AUTH" -X 'UID SEARCH ALL' | tr -d '\r')"

"$BIN/letterdesk-desktop" --mailbox "$WORK/mail" --account "$WORK/account.json" --my-name "Lin Chen" >"$WORK/app.log" 2>&1 &
APP=$!
WID="$(timeout 60 xdotool search --sync --name '^Letterdesk$' | head -1 || true)"
if [ -z "$WID" ]; then
  echo "失败：60 秒内没有出现 Letterdesk 窗口" >&2
  tail -20 "$WORK/app.log" >&2 || true
  exit 1
fi
sleep 5

# 写信框在右栏中部；“发送”在收件人、抄送下方。
xdotool mousemove --window "$WID" 1075 500 click 1
sleep 0.5
xdotool type --delay 20 "Thursday at 3 pm works for me."
sleep 0.5
xdotool mousemove --window "$WID" 914 131 click 1

for _ in $(seq 1 20); do
  after="$(curl -s --noproxy '*' "$IMAP" -u "$AUTH" -X 'UID SEARCH ALL' | tr -d '\r')"
  [ "$after" != "$before" ] && break
  sleep 1
done
last="$(echo "$after" | awk '{print $NF}')"
if [ "$after" = "$before" ] || [ -z "$last" ] || [ "$last" = "SEARCH" ]; then
  echo "失败：点击“发送”后“已发送”里没有新邮件" >&2
  tail -20 "$WORK/app.log" >&2 || true
  exit 1
fi
headers="$(curl -s --noproxy '*' "$IMAP;UID=$last" -u "$AUTH" | tr -d '\r')"
for want in "In-Reply-To: <maya-html-1@example.com>" "To: \"Maya Patel\" <maya@example.com>" "Cc: \"Alex Carter\" <alex@example.com>" "Thursday at 3 pm works for me."; do
  if ! grep -qF "$want" <<<"$headers"; then
    echo "失败：回信中缺少：$want" >&2
    echo "$headers" | head -20 >&2
    exit 1
  fi
done
echo "通过：回信已发出，线程信息与收件人正确"
