#!/usr/bin/env sh
set -eu

HOST="${HOST:-127.0.0.1}"
PORT="${PORT:-25}"
FROM="${FROM:-sender@example.com}"
TO="${TO:-receiver@example.com}"
SUBJECT="${SUBJECT:-mail-box test}"

MESSAGE="${MESSAGE:-This is a test email sent by test-send-mail.sh}"

send_commands() {
  printf 'EHLO localhost\r\n'
  printf 'MAIL FROM:<%s>\r\n' "$FROM"
  printf 'RCPT TO:<%s>\r\n' "$TO"
  printf 'DATA\r\n'
  printf 'From: <%s>\r\n' "$FROM"
  printf 'To: <%s>\r\n' "$TO"
  printf 'Subject: %s\r\n' "$SUBJECT"
  printf '\r\n'
  printf '%s\r\n' "$MESSAGE"
  printf '.\r\n'
  printf 'QUIT\r\n'
}

if command -v nc >/dev/null 2>&1; then
  send_commands | nc "$HOST" "$PORT"
elif command -v telnet >/dev/null 2>&1; then
  send_commands | telnet "$HOST" "$PORT"
else
  printf 'Error: install nc or telnet to use this script.\n' >&2
  exit 1
fi
