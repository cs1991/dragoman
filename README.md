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

### Windows login and packaging

On Windows, `dragoman.exe login` looks for Chrome in the standard installation directories and on `PATH`. It creates `chrome_profile` beside `dragoman.exe`, so place the executable in a writable directory. If Chrome is already listening on the selected CDP port with an older profile, close that Chrome instance before logging in again. Dragoman saves its session under `%USERPROFILE%\.config\dragoman\session.json` when `HOME` is unset. Keep this file private.

Run the packaging script from Windows Command Prompt with Cargo on `PATH` or in the default Rustup location.

```bat
.\scripts\package.cmd
```

The script stops running instances from `target/release` and `dist/dragoman-windows` before building. It copies the executable, launch scripts, README, and license into `dist/dragoman-windows` without making a ZIP. It leaves any existing browser profiles in both directories intact.

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

Streaming requests, `input_video` parts and `file` parts use the same upload path. A request carries at most 10 attachments. `--max-attachment-mb` sets the per-attachment size limit and defaults to 200.

Media parts without a usable source return HTTP 400. This prevents base64 payloads or attachment objects from becoming oversized prompt text.

The Gemini account must accept the video upload notice once. Send one video from `gemini.google.com` if a video request is blocked.

---

## CLI Reference

```text
Usage: dragoman [COMMAND]

Commands:
  serve     Start the standalone OpenAI-compatible proxy server (no browser needed)
  login     Perform one-time browser login to extract and save session credentials
  test-rpc  Test direct native HTTP RPC without browser
  help      Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

`serve --max-attachment-mb 200` changes the per-attachment size limit.

`start.cmd` stops a previous Dragoman process using the same executable and port before it starts.

---

## Project Structure

```text
dragoman/
├── Cargo.toml          # Rust crate manifest
├── LICENSE             # MIT License
├── README.md           # Documentation
└── src/
    ├── main.rs         # CLI entry point (serve, login, test-rpc)
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
