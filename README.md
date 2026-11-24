# Dragoman

> Dragoman: A local OpenAI-compatible bridge written in Rust. Routes requests from coding agents (Cline, Aider, OpenHands, Codex) to Gemini Web (`gemini.google.com`) to query the Gemini 4 Argon model with a Google AI Pro subscription.

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/Rust-2024_Edition-orange.svg)](https://www.rust-lang.org/)

---

## Core Features

- Direct Gemini 4 Argon access: Routes queries to Gemini 4 Argon (`Argon4p0`) through web sessions.
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
  ├── Targets internal model: Argon4p0 (Gemini 4 Argon)
  │
  ▼
Real-Time Stream Parser
  │
  └── Emits standard OpenAI SSE chunks to the agent
```

> Browser usage: Chrome runs only once during `dragoman login` to read session cookies and the anti-CSRF token (`SNlM0e`). Dragoman saves these credentials locally and shuts down Chrome.

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
3. Dragoman reads the session tokens and saves them to `~/.config/dragoman/session.json`.
4. Close Chrome when prompted.

### 3. Verify Direct RPC Connection

Test communication with Gemini 4 Argon:

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

Open `dist\dragoman-windows\DragomanGui.exe`. Keep `dragoman.exe` beside it in a writable directory. Add an account by entering a local alias. Chrome opens a normal window with no DevTools endpoint, because Google rejects sign-in while remote debugging is enabled. Sign in to Google and open Gemini. Dragoman watches for the Google session cookies, restarts that profile with DevTools for a few seconds, reads the session and saves it, then closes that window. Each desktop account has its own Chrome profile under `desktop-profiles` beside the executables. For example, `work` uses `desktop-profiles\account-work.profile`. Keep the profile directories private. The desktop list also shows existing CLI and legacy sessions.

Use the account table to sign in again, enable, disable, or delete a saved proxy session. Deletion retains the Chrome profile. Stop the proxy before changing accounts. If you also run `dragoman serve` separately, stop that instance before changing its sessions.

Use **启动** to start the local proxy and **复制 API 地址** to copy its base URL. The desktop proxy listens on `127.0.0.1` only. Closing the app stops the proxy it started. The existing CLI remains available for headless use.

### Windows CLI login and packaging

On Windows, `dragoman.exe login` looks for Chrome in the standard installation directories and on `PATH`. It creates `chrome_profile` beside `dragoman.exe`, so place the executable in a writable directory. If Chrome is already listening on the selected CDP port with an older profile, close that Chrome instance before logging in again. Dragoman saves its session under `%USERPROFILE%\.config\dragoman\session.json` when `HOME` is unset. Keep this file private.

Run the packaging script from Windows Command Prompt with Cargo on `PATH` or in the default Rustup location.

```bat
.\scripts\package.cmd
```

The script stops running CLI instances from `target/release` and `dist/dragoman-windows` before building. It copies both executables, launch scripts, README, and license into `dist/dragoman-windows` without making a ZIP. It leaves any existing browser profiles in both directories intact.

In the output folder, double-click `login.cmd` for first-time setup. Finish signing in to Gemini in Chrome. If `login.cmd` did not save a session on the first run, leave Chrome open and run it again. After it reports success, close that Chrome window and double-click `start.cmd`. The startup window shows the error if the proxy exits.

Dragoman honors the Windows system proxy for its connection to Gemini. If Gemini opens in Chrome but Dragoman reports a connection error, check that the proxy is running and enabled in Windows Settings. A proxy set only inside Chrome does not apply to Dragoman. When starting Dragoman from a terminal, you can also set `HTTPS_PROXY` for that process.

---

## Agent Configuration

Point your AI coding tool to Dragoman with these settings:

| Setting | Value |
| :--- | :--- |
| **API Base URL** | `http://127.0.0.1:8080/v1` |
| **Model** | `gemini-4-argon` |
| **API Key** | Leave blank. If the client requires a value, use `dragoman-local`. |

Dragoman accepts requests with or without an `Authorization` header and does not check the API key. The proxy still needs the Gemini session saved by `dragoman login`. It listens on all IPv4 interfaces, so keep the port behind your local firewall and do not forward it.

### Example: Non-Streaming Request

```bash
curl http://127.0.0.1:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gemini-4-argon",
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
    "model": "gemini-4-argon",
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
    "model": "gemini-4-argon",
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
    "model": "gemini-4-argon",
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
