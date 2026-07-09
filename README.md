# mail-box

`mail-box` is a small Rust CLI SMTP receiver for local testing, debugging, and collecting inbound email traffic. It listens on TCP port `25` by default, accepts plaintext SMTP, supports opportunistic `STARTTLS`, and accepts all senders and recipients.

This tool is intentionally permissive. It is not a secure production mail server and does not perform authentication, relay policy checks, spam filtering, DKIM/SPF validation, or mailbox delivery.

## Features

- Listen for SMTP on `0.0.0.0:25` by default.
- Accept plaintext SMTP.
- Accept `STARTTLS` using a self-signed certificate generated at startup.
- Accept all `MAIL FROM` senders.
- Accept all `RCPT TO` recipients.
- Save the full SMTP conversation transcript to disk by default.
- Send received email data to a webhook.
- Print debug logs to stdout.
- Save received email data to Firebase Realtime Database.
- Save received email data to MongoDB.
- Automatically delete old received data from enabled storage modes.

## Build

Build a release binary with the included script:

```sh
./build-release.sh
```

The release binary will be created at:

```text
target/release/mail-box
```

Build for the current machine explicitly:

```sh
./build-release.sh --native
```

Build one target triple:

```sh
./build-release.sh --target x86_64-unknown-linux-gnu
```

Build common Linux, Windows, and macOS amd64/arm64 targets:

```sh
./build-release.sh --all
```

Common targets:

- `x86_64-unknown-linux-gnu`: Linux amd64 glibc
- `aarch64-unknown-linux-gnu`: Linux arm64 glibc
- `x86_64-unknown-linux-musl`: Linux amd64 static/musl
- `aarch64-unknown-linux-musl`: Linux arm64 static/musl
- `x86_64-pc-windows-gnu`: Windows amd64
- `aarch64-pc-windows-gnullvm`: Windows arm64
- `x86_64-apple-darwin`: macOS amd64
- `aarch64-apple-darwin`: macOS arm64

Cross-compilation works best with `cargo-zigbuild` and `zig` installed:

```sh
cargo install cargo-zigbuild
```

Native builds only require `cargo`. macOS targets normally require Apple's SDK/toolchain and are best built on macOS.

## Run

Run with defaults:

```sh
sudo ./target/release/mail-box
```

Port `25` usually requires root privileges on Linux. For local testing without `sudo`, bind to a high port:

```sh
./target/release/mail-box --bind 127.0.0.1:2525
```

## Basic Usage Flow

1. Start `mail-box`.
2. An SMTP client connects to the configured TCP address.
3. The server sends `220 mail-box ready`.
4. The client sends `EHLO` or `HELO`.
5. The server responds with SMTP capabilities, including `STARTTLS` when TLS has not been enabled yet.
6. If the client sends `STARTTLS`, the connection is upgraded to TLS using a generated self-signed certificate.
7. The client sends `MAIL FROM`, `RCPT TO`, and `DATA`.
8. The server accepts the message and replies with `250 message accepted`.
9. After the message is accepted, enabled processing modes run: transcript saving, webhook delivery, Firebase saving, and MongoDB saving.
10. A cleanup task runs immediately when the app starts, then repeats every 5 minutes by default.
11. The client may send `QUIT` to close the SMTP session.

Unknown SMTP commands receive `250 ok` so the server remains permissive and avoids rejecting messages from clients that send extra commands.

## Test Sending Mail

Use the included test script:

```sh
./test-send-mail.sh
```

The script uses `nc` when available and falls back to `telnet`.

Test against a custom host or port:

```sh
HOST=127.0.0.1 PORT=2525 ./test-send-mail.sh
```

Customize the test email:

```sh
HOST=127.0.0.1 \
PORT=2525 \
FROM=alice@example.com \
TO=bob@example.com \
SUBJECT="Hello" \
MESSAGE="This is a test message" \
./test-send-mail.sh
```

## Options

### Bind Address

Default:

```text
0.0.0.0:25
```

CLI:

```sh
--bind 127.0.0.1:2525
```

Environment variable:

```sh
MAIL_BOX_BIND=127.0.0.1:2525
```

### Transcript Saving

Transcript saving is enabled by default.

Default directory:

```text
save-transcript
```

Each transcript file contains the full SMTP conversation with client and server lines. File names use this format:

```text
sender-recipient-time.txt
```

Special characters are converted to `-`, and the generated file name does not contain spaces.

Change the transcript directory:

```sh
--transcript-dir ./my-transcripts
```

Environment variable:

```sh
MAIL_BOX_TRANSCRIPT_DIR=./my-transcripts
```

Disable transcript saving:

```sh
--no-save-transcript
```

Environment variable:

```sh
MAIL_BOX_NO_SAVE_TRANSCRIPT=true
```

### Debug Mode

Print SMTP conversation and processing logs to stdout:

```sh
--debug
```

Environment variable:

```sh
MAIL_BOX_DEBUG=true
```

### Webhook Mode

Create a JSON config file:

```json
{
  "url": "https://example.com/webhook"
}
```

Run with webhook enabled:

```sh
./target/release/mail-box --webhook-config webhook.json
```

Environment variable:

```sh
MAIL_BOX_WEBHOOK_CONFIG=webhook.json
```

After an email is accepted, `mail-box` sends a `POST` request to the configured webhook URL with a JSON body containing the received email data.

### Firebase Realtime Database Mode

Run with Firebase enabled:

```sh
./target/release/mail-box \
  --firebase-url https://your-project.firebaseio.com \
  --firebase-path emails
```

If your Firebase write rules require authentication:

```sh
./target/release/mail-box \
  --firebase-url https://your-project.firebaseio.com \
  --firebase-auth YOUR_FIREBASE_TOKEN
```

Environment variables:

```sh
MAIL_BOX_FIREBASE_URL=https://your-project.firebaseio.com
MAIL_BOX_FIREBASE_AUTH=YOUR_FIREBASE_TOKEN
MAIL_BOX_FIREBASE_PATH=emails
```

The server writes Firebase data to three Realtime Database areas:

```text
messages/{firebase-path}/{message-id}
messageSummaries/{firebase-path}/{message-id}
messageGroups/{firebase-path}
messageCleanup/{firebase-path}/{message-id}
```

`messages` stores the full payload, `messageSummaries` stores lightweight list data, `messageGroups` stores realtime group metadata such as message count and the latest message id, and `messageCleanup` stores only `received_at` timestamps for public-read cleanup scans.

Recommended Firebase Realtime Database rules for the companion web app:

```json
{
  "rules": {
    ".read": "auth != null",
    "messageCleanup": {
      ".read": true
    },
    ".write": true
  }
}
```

With these rules, full message data and list summaries are only readable by signed-in Firebase users. `messageCleanup` is intentionally public-read because it contains only retention metadata, not email content. This lets `mail-box` scan for expired records without reading `messages`.

If you change `.write` to require auth, provide `--firebase-auth` or `MAIL_BOX_FIREBASE_AUTH` with a token that can write and delete all four Firebase paths.

### MongoDB Mode

Run with MongoDB enabled:

```sh
./target/release/mail-box \
  --mongodb-uri mongodb://localhost:27017 \
  --mongodb-database mail_box \
  --mongodb-collection emails
```

Environment variables:

```sh
MAIL_BOX_MONGODB_URI=mongodb://localhost:27017
MAIL_BOX_MONGODB_DATABASE=mail_box
MAIL_BOX_MONGODB_COLLECTION=emails
```

### Automatic Cleanup

Automatic cleanup is enabled by default. It runs once immediately when the app starts, then repeats every 5 minutes.

Default cleanup behavior:

- Interval: 5 minutes
- Retention: 30 minutes
- Transcript files: deletes old files from the configured transcript directory when transcript saving is enabled
- Firebase: reads public cleanup metadata, deletes old full records, summaries, and cleanup metadata from the configured Firebase path when Firebase mode is enabled, then decrements the group count
- MongoDB: deletes old records from the configured database and collection when MongoDB mode is enabled
- Webhook: no cleanup is performed because webhook delivery is not stored locally by `mail-box`

Configure the cleanup interval:

```sh
--cleanup-interval-minutes 5
```

Configure the retention period:

```sh
--cleanup-retention-minutes 30
```

Disable automatic cleanup:

```sh
--no-cleanup
```

Environment variables:

```sh
MAIL_BOX_CLEANUP_INTERVAL_MINUTES=5
MAIL_BOX_CLEANUP_RETENTION_MINUTES=30
MAIL_BOX_NO_CLEANUP=true
```

Firebase cleanup reads `messageCleanup/{firebase-path}`, filters records by `received_at` locally, and deletes matching keys one by one from `messages`, `messageSummaries`, and `messageCleanup`. It also decrements `messageGroups/{firebase-path}/count`. MongoDB cleanup deletes records where `received_at` is older than the retention cutoff. Transcript cleanup uses each file's modified time.

## Stored Payload

Webhook, Firebase, and MongoDB modes use the same payload shape:

```json
{
  "peer_addr": "127.0.0.1:54321",
  "from": "sender@example.com",
  "recipients": ["receiver@example.com"],
  "data": "From: <sender@example.com>\nTo: <receiver@example.com>\nSubject: test\n\nHello\n",
  "transcript": "S: 220 mail-box ready\r\nC: EHLO localhost\r\n...",
  "received_at": "2026-07-08T12:00:00Z"
}
```

## Example Commands

Run locally on port `2525` with debug logs:

```sh
./target/release/mail-box --bind 127.0.0.1:2525 --debug
```

Run on port `25`, save transcripts to the default directory, and enable webhook:

```sh
sudo ./target/release/mail-box --webhook-config webhook.json
```

Run without saving transcripts:

```sh
./target/release/mail-box --bind 127.0.0.1:2525 --no-save-transcript
```

Run with all processing modes enabled:

```sh
sudo ./target/release/mail-box \
  --debug \
  --cleanup-interval-minutes 5 \
  --cleanup-retention-minutes 30 \
  --webhook-config webhook.json \
  --firebase-url https://your-project.firebaseio.com \
  --firebase-path emails \
  --mongodb-uri mongodb://localhost:27017 \
  --mongodb-database mail_box \
  --mongodb-collection emails
```

## Notes

- `STARTTLS` uses a self-signed certificate generated on startup.
- This server is designed to receive and inspect messages, not to relay or deliver mail to real inboxes.
- Binding to port `25` may require `sudo` or Linux capabilities.
- If you change the source code, rebuild the release binary with `./build-release.sh`.
