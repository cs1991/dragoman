# Dragoman

> Dragoman: A local OpenAI-compatible bridge written in Rust. Routes requests from coding agents (Cline, Aider, OpenHands, Codex) to Gemini Web (`gemini.google.com`) to query the Gemini 4 models with a Google AI Pro subscription.

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/Rust-2024_Edition-orange.svg)](https://www.rust-lang.org/)

---

## Core Features

- Direct Gemini 4 access: Routes queries to Hydrogen, Auto, or Neon through web sessions. The model is selected per request.
- Zero browser runtime: Uses native HTTP/2 direct RPC (`StreamGenerate`). Chrome does not run in the background during normal use.
- Standard OpenAI API compatibility: Implements `/v1/chat/completions`, `/v1/models`, and `/health`.
- Real-time streaming: Server-Sent Events (SSE) deliver chunked token responses as they generate.
- Attachment analysis: video, image and file content parts are uploaded and attached to the Gemini request.
- Tool calling translation: Formats tool schemas into instructions and parses `<tool_call>` outputs into standard OpenAI `tool_calls` structures.
- No API tokens required: Works with personal Google AI Pro subscriptions without Google Cloud billing.

---

## Architecture

```text
External Coding Agent (Cline / Aider / OpenHands / Codex)
  │
  │ HTTP POST /v1/chat/completions (OpenAI JSON + Tool Schemas)
  ▼
Dragoman Proxy Server (Rust / Axum on http://127.0.0.1:8080)
  │
  ├── 1. PromptTranslator: Maps chat history and tool schemas into prompts
  ├── 2. GeminiWebClient: Direct HTTP/2 with OpenSSL
  │      └── Loads session tokens from ~/.config/dragoman/session.json
  ▼
Google Gemini Backend (assistant.lamda.BardFrontendService/StreamGenerate)
  │
  ├── Targets Gemini 4 model from the request: Hydrogen (default), Auto, Neon
  │
  ▼
Real-Time Stream Parser
  │
  └── Emits standard OpenAI SSE chunks to the agent
```

> Browser usage: Chrome runs only during `dragoman login` to read session cookies and the anti-CSRF token (`SNlM0e`). Dragoman saves these credentials locally and leaves the login window open.

---

## Quickstart

### 1. Build from Source

Requirements: Rust edition 2024 (version 1.85 or later).

```bash
git clone https://github.com/khoipm06/dragoman.git
cd dragoman
cargo build --release
```

The compiled binary is written to `./target/release/dragoman`.

### 2. Initial Authentication

Run the login command once to link your Google AI Pro account:

```bash
./target/release/dragoman login
```

Steps performed:
1. Dragoman starts Chrome with remote debugging.
2. Log in at [gemini.google.com](https://gemini.google.com).
3. Dragoman watches the profile and detects the completed Google sign-in.
4. Dragoman reads the session tokens and saves them to `~/.config/dragoman/session.json`.
5. The Chrome window stays open, close it whenever you want.

### 3. Verify Direct RPC Connection

Test communication with the default model:

```bash
./target/release/dragoman test-rpc "State your model identity in one sentence."
```

### 4. Start the Standalone Proxy Server

Launch the proxy server:

```bash
./target/release/dragoman serve --port 8080
```

Connect from the same machine at `http://127.0.0.1:8080`.

### Windows desktop app

Build the Windows package with Cargo and the Windows .NET Framework compiler.

```cmd
scripts\package.cmd
```

Open `dist\dragoman-windows\DragomanGui.exe`. Keep `dragoman.exe` beside it in a writable directory. Add an account by entering a local alias. Chrome opens one window and Dragoman watches that window for the whole sign-in. The session is read and saved the moment the Google cookies appear, so the account lands in the table while Chrome stays open. DevTools is attached only after the sign-in cookies exist, which keeps the login page clear of an active debugger. Each desktop account has its own Chrome profile under `desktop-profiles` beside the executables. For example, `work` uses `desktop-profiles\account-work.profile`. Keep the profile directories private. The desktop list also shows existing CLI and legacy sessions.

Use the account table to sign in again, enable, disable, or delete a saved proxy session. Deletion retains the Chrome profile. Stop the proxy before changing accounts. If you also run `dragoman serve` separately, stop that instance before changing its sessions.

Use **启动** to start the local proxy and **复制 API 地址** to copy its base URL. The desktop proxy listens on `127.0.0.1` only. Closing the app stops the proxy it started. The existing CLI remains available for headless use.

### Windows CLI login and packaging

On Windows, `dragoman.exe login` looks for Chrome in the standard installation directories and on `PATH`. It creates `chrome_profile` beside `dragoman.exe`, so place the executable in a writable directory. If Chrome is already listening on the selected CDP port with an older profile, close that Chrome instance before logging in again. Dragoman saves its session under `%USERPROFILE%\.config\dragoman\session.json` when `HOME` is unset. Keep this file private.

Run the packaging script from Windows Command Prompt with Cargo on `PATH` or in the default Rustup location.

```bat
.\scripts\package.cmd
```

The script stops running CLI instances from `target/release` and `dist/dragoman-windows` before building. It copies both executables, launch scripts, README, and license into `dist/dragoman-windows` without making a ZIP. It leaves any existing browser profiles in both directories intact.

In the output folder, double-click `login.cmd` for first-time setup. Finish signing in to Gemini in Chrome. `login.cmd` saves the session as soon as the sign-in completes and keeps the Chrome window open. After it reports success, double-click `start.cmd`. The startup window shows the error if the proxy exits.

Dragoman uses the Windows system proxy when an account has no `proxy` field. If Gemini opens in Chrome but Dragoman reports a connection error, check that the proxy is running and enabled in Windows Settings. A proxy set only inside Chrome does not apply to Dragoman. When starting Dragoman from a terminal, you can also set `HTTPS_PROXY` for that process. A per-account proxy overrides both values. The next section describes the field.

## Per-Account Egress Proxy

Each saved session can carry its own egress route. Dragoman builds one HTTP client per account and sends both attachment uploads and `StreamGenerate` calls through that route. Gemini sees the address of the proxy node instead of the Windows system proxy.

The session fields are `proxy` and `local_ip`.

```json
{
  "proxy": "http://127.0.0.1:7897",
  "local_ip": "127.0.0.7"
}
```

The proxy port stays at the single Clash Verge mixed port, 7897. `local_ip` binds the client socket to one loopback address and Clash picks the node with a `SRC-IP-CIDR` rule that matches that address. Fifty accounts can share port 7897 and still leave through fifty different nodes. Sessions that name the same `local_ip` share one public address. A bare `127.0.0.1:7897` is accepted. `dragoman accounts` prints the proxy in the PROXY column and the source address in the LOCAL IP column.

### Clash Verge source rules

Clash Verge 2.4.7 copies unknown Merge keys into the generated config without applying them, so the rules go into the global Script instead. The Script runs on every profile activation and on every subscription update.

```javascript
const dragomanSrcRules = [
  "SRC-IP-CIDR,127.0.0.2/32,🇦🇷 Argentina | 01",
  "SRC-IP-CIDR,127.0.0.3/32,🇦🇺 Australia | 01",
];

function main(config, profileName) {
  const rules = Array.isArray(config.rules) ? config.rules : [];
  const known = new Set(rules);
  const missing = dragomanSrcRules.filter((rule) => !known.has(rule));
  config.rules = missing.concat(rules);
  return config;
}
```

`scripts\windows\clash-src-rules.ps1` reads the running Clash controller, takes the real node names and writes the Script for you. It reads the controller secret from the Clash Verge `config.yaml` when `-Secret` is omitted. The old `prepend-rules` and `listeners` blocks are removed from Merge.yaml.

```powershell
# 50 accounts, one source address and one node per account
powershell -ExecutionPolicy Bypass -File scripts\windows\clash-src-rules.ps1 -StartIp 2 -Count 50 -AccountsPerIp 1 -Write

# 10 nodes, five accounts per node and per address
powershell -ExecutionPolicy Bypass -File scripts\windows\clash-src-rules.ps1 -StartIp 2 -Count 10 -AccountsPerIp 5 -Write

# keep only nodes that answer a probe
powershell -ExecutionPolicy Bypass -File scripts\windows\clash-src-rules.ps1 -StartIp 2 -Count 50 -HealthyOnly -Write

# only Japan and Singapore nodes
powershell -ExecutionPolicy Bypass -File scripts\windows\clash-src-rules.ps1 -StartIp 2 -Count 20 -NodePattern "Japan|Singapore" -Write
```

In Clash Verge open the profile page and click the flame icon at the top right, 重新激活订阅. Then check two source addresses.

```powershell
curl.exe --interface 127.0.0.2 -x http://127.0.0.1:7897 https://api.ipify.org
curl.exe --interface 127.0.0.3 -x http://127.0.0.1:7897 https://api.ipify.org
```

Two different addresses prove that one port routes to different nodes by source address.

### Assign proxies

Desktop app. Set **每 IP 账号** before adding accounts. The account dialog pre-fills the fixed proxy `http://127.0.0.1:7897`. **设置代理** changes one account and takes both the proxy and the source address. **批量分配代理** assigns source addresses in account-name order. Enter the first `127.0.0.x` octet, the shared port and the number of accounts per address. The assignment is written into the session files and takes effect on the next **启动**.

CLI.

```powershell
dragoman login --account acc01 --proxy http://127.0.0.1:7897 --profile-dir D:\chrome-profiles\acc01
dragoman accounts
dragoman serve --port 8080 --balance least-used
```

`login --proxy` also starts Chrome with `--proxy-server`, so the sign-in request and later RPC calls leave from the same port. Chrome itself uses the system source address, so pick the account node in Clash before signing in, or assign the source address first and set the Clash rule before the first RPC call.

The management commands are available for scripts.

```powershell
dragoman desktop-accounts assign-proxies --start-ip=2 --proxy-port=7897 --per-ip=1
dragoman desktop-accounts set-proxy --id=named:acc01 --proxy=http://127.0.0.1:7897 --local-ip=127.0.0.2
dragoman desktop-accounts set-proxy --id=named:acc01 --proxy=
```

`assign-proxies` orders accounts by name. `--start-ip=2 --per-ip=1` gives account 1 `127.0.0.2`, account 2 `127.0.0.3` and so on, all through 7897. `--per-ip=5` puts five consecutive accounts on one source address and one node. `set-proxy` with an empty value clears the route and returns the account to the Windows system proxy.

### Why the client disables the system proxy

`reqwest` reads the Windows proxy settings by default. A per-account client calls `no_proxy()` before it adds its own proxy, so one account does not inherit the registry proxy and does not share an address with the others.

`local_address` binds the socket before the TCP connect, so the Clash listener sees the account address as the connection source. Windows accepts every address in `127.0.0.0/8` as a loopback source without adding it to a network adapter.

---

## Agent Configuration

Point your AI coding tool to Dragoman with these settings:

| Setting | Value |
| :--- | :--- |
| **API Base URL** | `http://127.0.0.1:8080/v1` |
| **Model** | `gemini-4-hydrogen` (default), `gemini-4-auto`, `gemini-4-neon` |
| **API Key** | Leave blank. If the client requires a value, use `dragoman-local`. |

Dragoman accepts requests with or without an `Authorization` header and does not check the API key. The proxy still needs the Gemini session saved by `dragoman login`. It listens on all IPv4 interfaces, so keep the port behind your local firewall and do not forward it.

### Model Selection

`/v1/models` lists the models the signed-in accounts can use. Pass one of the ids in the `model` field of `/v1/chat/completions`.

| Model | Upstream id | Behaviour |
| :--- | :--- | :--- |
| `gemini-4-hydrogen` | `56fdd199312815e2` | Everyday help, lowest latency. This is the default. |
| `gemini-4-auto` | `8c46e95b1a07cecc` | Gemini picks the depth per prompt. |
| `gemini-4-neon` | `e6fa609c3fa255c0` | Deeper analysis. It reasons longer and runs code for arithmetic. |

The legacy names `gemini-4-argon` and `gemini-4-argon-thinking` still resolve. They map to Hydrogen and Neon. An unknown name falls back to Hydrogen.

`gemini-4-carbon` answers 400 with `model_not_available`. The web app shows Carbon behind a Google AI Ultra upgrade, so a Pro session cannot generate with it.

Model choice travels in the `x-goog-ext-525001261-jspb` header. The same number is mirrored into payload slot 79.

When Gemini Web parks a request the stream carries `{"37":[n]}` queue heartbeats and no candidate text. Dragoman stops after three heartbeats and answers 503 with `model_not_available` instead of holding the connection open. Set `DRAGOMAN_PARKED_TICKS` to change the threshold.

### Example: Non-Streaming Request

```bash
curl http://127.0.0.1:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gemini-4-hydrogen",
    "messages": [
      {"role": "user", "content": "Explain what a dragoman was in history in 20 words."}
    ],
    "stream": false
  }'
```

### Example: Streaming Request (SSE)

```bash
curl -N http://127.0.0.1:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gemini-4-neon",
    "messages": [
      {"role": "user", "content": "Write a Rust function that computes Fibonacci numbers."}
    ],
    "stream": true
  }'
```

### Video and File Attachments

`/v1/chat/completions` accepts `video_url`, `input_video`, `image_url` and `file` content parts. Each part can carry a data URL, an HTTP or HTTPS URL, an absolute local path, a `file://` URL, or an existing `/contrib_service/` upload path. Video and image parts also accept a `source` object with `media_type` plus `data`, `url`, or `file_id`.

Local video file:

```bash
curl http://127.0.0.1:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gemini-4-neon",
    "messages": [{
      "role": "user",
      "content": [
        {"type": "text", "text": "Describe this video"},
        {"type": "video_url", "video_url": {"url": "file:///D:/clips/demo.mp4"}}
      ]
    }]
  }'
```

Base64 data URL with Python:

```bash
python - <<'PY'
import base64
import json
import pathlib
import urllib.request

video = base64.b64encode(pathlib.Path("clip.mp4").read_bytes()).decode("ascii")
body = {
    "model": "gemini-4-neon",
    "messages": [{
        "role": "user",
        "content": [
            {"type": "text", "text": "Describe this video"},
            {"type": "video_url", "video_url": {"url": f"data:video/mp4;base64,{video}"}},
        ],
    }],
}
request = urllib.request.Request(
    "http://127.0.0.1:8080/v1/chat/completions",
    data=json.dumps(body).encode(),
    headers={"Content-Type": "application/json"},
)
print(urllib.request.urlopen(request, timeout=600).read().decode())
PY
```

Streaming requests, `input_video` parts and `file` parts use the same upload path. Attachment count per request is not capped, and `--max-attachment-mb` sets the per-attachment size limit with a default of 200. The HTTP body limit is derived from that value, so a larger limit accepts a larger total request.

`max_tokens` is accepted and ignored. Gemini Web decides when a response ends, so the proxy never cuts the answer short. The full message history is sent as prompt text without truncation. The model context window belongs to Gemini itself.

Media parts without a usable source return HTTP 400. This prevents base64 payloads or attachment objects from becoming oversized prompt text.

Clients that omit the part `type` field still work when the part carries `image_url`, `video_url`, `input_image`, `input_video`, `file`, `input_file`, or a `source` object with `media_type`. A data URL embedded in plain message text is lifted out of the prompt, uploaded, and replaced with `[attached image]`, `[attached video]`, or `[attached file]`. The request log prints `Request shape` with part types, text length, and the number of inline media items, never the payload bytes.

The proxy clears its stored `conversation_id` and `response_id` when a turn fails or returns no text, then retries once with a fresh conversation. This removes the stuck state that follows an aborted or stalled Gemini turn. The stream watchdog aborts a turn after 120 seconds without upstream data and defaults can be changed with `DRAGOMAN_STREAM_IDLE_TIMEOUT_SECS`.

HEVC and H.265 video is transcoded to H.264 with `ffmpeg` before upload. Gemini Web queues HEVC clips without ever streaming an answer, while the same clip as H.264 answers in seconds. The transcode uses `libx264 -preset veryfast -crf 23` plus AAC audio, keeps a temporary file in the system temp directory, and removes it after the upload. Set `DRAGOMAN_TRANSCODE=0` to upload the original bytes. `ffmpeg` and `ffprobe` must be on PATH for this step.

The Gemini account must accept the video upload notice once. Send one video from `gemini.google.com` if a video request is blocked.

---

## Multiple Accounts and Load Balancing

Every `*.json` file under `~/.config/dragoman/sessions` is one account. The legacy `~/.config/dragoman/session.json` loads as the account named `default`. Files with identical cookies are collapsed so one Google account is not counted twice.

```bash
dragoman login --account work
dragoman login --account backup
dragoman serve --balance round-robin
```

`login` writes `sessions/<name>.json` and prints the full path. Account names accept letters, digits, dash, underscore and dot.

Chrome keeps one signed-in Google account per profile. Give every account its own profile directory and CDP port.

```bash
dragoman login --account work --profile-dir D:/chrome-profiles/work --cdp-port 9223
dragoman login --account backup --profile-dir D:/chrome-profiles/backup --cdp-port 9224
```

### Batch login

`login-batch` signs in a list of Google accounts and writes one session file per account. The line format is `email----password---`, one account per line. A third field is accepted as `email----password----2FA密钥`, where the last field is the 32 character base32 authenticator secret. Line breaks may be LF, CRLF or CR, and files may be UTF-8 or UTF-16. A file whose line breaks were lost is also split at each mailbox address.

```bash
dragoman login-batch --file accounts.txt
dragoman login-batch --text "first@example.asia----pass-one---\nsecond@example.asia----pass-two---"
```

With no `--file` and no `--text`, the command reads standard input. Paste the accounts, press Ctrl+Z, then press Enter. Passwords stay out of the process list when a file or standard input is used.

Every account gets a Chrome profile under `desktop-profiles/batch-<name>.profile`. The profile keeps the trusted-device cookie, so a later run signs in with fewer prompts. Sessions are saved to `~/.config/dragoman/sessions/<name>.json` and are ready for `dragoman serve`.

When the account line carries an authenticator secret, Dragoman computes a six digit TOTP code locally and submits it first. When the page still asks for a code, or the line has no secret, Dragoman posts the account email to `https://google111111.asia/` and reads the newest six digit code from that inbox. The wait limit is 120 seconds and can be changed with `--code-timeout`.

```bash
dragoman login-batch --file accounts.txt --proxy http://127.0.0.1:7901
dragoman login-batch --file accounts.txt --start 11 --limit 10
dragoman login-batch --file accounts.txt --dry-run
```

`--proxy` applies to the Chrome sign-in and is saved into every session, so the later RPC uses the same egress. `--start` and `--limit` select a slice for resuming a long list. `--dry-run` parses the list and prints the account count without opening Chrome. A failed account leaves a screenshot in `~/.config/dragoman/batch-login-shots`. The final report is `~/.config/dragoman/batch-login-report.json`.

The Windows desktop app has a `批量登录` button next to `新增账号`. It accepts pasted text and `.txt` files.

`serve` leases one account per turn. Media is uploaded with that account's cookie, then the prompt is sent on the same account. A failed turn parks the account for its cooldown and the next account retries with a fresh upload. Cooldowns are 300 seconds for quota errors, 1800 seconds for auth errors, 30 seconds for transport errors and 60 seconds for a stalled turn.

`--balance round-robin` rotates accounts in file order. `--balance least-used` picks the account with the fewest completed turns. An account that is already serving a turn is skipped when another account is free, so two clients can run in parallel on different accounts.

`GET /v1/accounts` returns the live pool state.

```json
{
  "strategy": "round-robin",
  "total_served": 12,
  "accounts": [
    {"name": "work", "model": "Argon4p0", "has_conversation": true, "served": 7, "failures": 0, "cooldown_seconds": 0, "last_error": null},
    {"name": "backup", "model": "Argon4p0", "has_conversation": false, "served": 5, "failures": 1, "cooldown_seconds": 24, "last_error": "transient: The connection failed."}
  ]
}
```

`dragoman accounts` prints the configured accounts, their model, served count and cooldown state. Counters live in the running process, so a freshly started CLI shows zero.

Each account keeps its own Gemini conversation context. A conversation that stalls is cleared and retried once before the account is parked.

---

## CLI Reference

```text
Usage: dragoman [COMMAND]

Commands:
  serve     Start the standalone OpenAI-compatible proxy server (no browser needed)
  login     Perform one-time browser login to extract and save session credentials
  login-batch  Sign in many Google accounts from a file or pasted text
  accounts  List configured accounts and their rotation state
  test-rpc  Test direct native HTTP RPC without browser
  help      Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

`serve --max-attachment-mb 200` changes the per-attachment size limit.

`serve --balance least-used` switches from round-robin to least-used selection.

`login --account NAME` selects the session file name, default `main`. `login --profile-dir DIR --cdp-port PORT` isolates the Chrome profile for each Google account.

`start.cmd` stops a previous Dragoman process using the same executable and port before it starts.

---

## Project Structure

```text
dragoman/
├── Cargo.toml          # Rust crate manifest
├── LICENSE             # MIT License
├── README.md           # Documentation
└── src/
    ├── main.rs         # CLI entry point (serve, login, accounts, test-rpc)
    ├── accounts.rs     # Account pool, rotation strategy and cooldown state
    ├── browser/        # Chrome launcher and CDP client (used only during login)
    │   ├── cdp.rs
    │   ├── launcher.rs
    │   └── mod.rs
    ├── openai/         # OpenAI API data structures and schemas
    │   ├── mod.rs
    │   └── types.rs
    ├── proxy/          # Axum HTTP proxy and tool translator
    │   ├── mod.rs
    │   ├── server.rs
    │   └── translator.rs
    ├── session.rs      # Session credential persistence
    └── web/            # Direct HTTP/2 StreamGenerate RPC client
        ├── client.rs
        ├── upload.rs
        └── mod.rs
```

---

## License

This project is licensed under the [MIT License](LICENSE).
