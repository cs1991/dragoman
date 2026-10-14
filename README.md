# Dragoman

> **Dragoman** (*from Ottoman Turkish / Arabic dragoman: an official interpreter and diplomatic guide*): A high-performance, local OpenAI-compatible bridge written in Rust that connects coding agents (Cline, Aider, OpenHands, Codex) directly to **Gemini Web (`gemini.google.com`)**, providing seamless access to the **Gemini 4 Argon** frontier reasoning model using your personal **Google AI Pro** subscription.

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/Rust-2024_Edition-orange.svg)](https://www.rust-lang.org/)

---

## Key Highlights

- **Direct Gemini 4 Argon Access**: Unlocks Google's frontier reasoning model (`Argon4p0`) via web subscription.
- **Zero Browser Runtime**: Operates completely headless using native HTTP/2 direct RPC (`StreamGenerate`). Chrome is **never** kept running in the background.
- **Full OpenAI API Compatibility**: Exposes standard `/v1/chat/completions`, `/v1/models`, and `/health` endpoints.
- **Real-Time Streaming**: Native Server-Sent Events (SSE) deliver chunked token responses in real time with low latency.
- **Agentic Tool Calling Emulation**: Injects function schemas into instructions and parses `<tool_call>` outputs into standard OpenAI `tool_calls` structures.
- **Zero Cloud Billing / API Keys**: Routes traffic through your web subscription without incurring per-token API charges.

---

## Architecture

```text
External Coding Agent (Cline / Aider / OpenHands / Codex)
  │
  │ HTTP POST /v1/chat/completions (OpenAI JSON + Function Schemas)
  ▼
Dragoman Proxy Server (Rust / Axum on http://127.0.0.1:8080)
  │
  ├── 1. PromptTranslator: Maps stateless turns & Tool Schemas into prompt
  ├── 2. GeminiWebClient: Direct HTTP/2 with OpenSSL (native-tls)
  │      └── Uses session tokens from ~/.config/dragoman/session.json
  ▼
Google Gemini Backend (assistant.lamda.BardFrontendService/StreamGenerate)
  │
  ├── Dispatches payload for: Argon4p0 (Gemini 4 Argon)
  │
  ▼
Real-Time Stream Parser
  │
  └── Emits standard OpenAI SSE chunks (`data: {"choices": [{"delta": ...}]}`) back to agent
```

> **Note on Browser Usage**: The browser is used **only once** during `dragoman login` to capture session cookies and the anti-CSRF token (`SNlM0e`). Once captured, the browser is terminated and never needed during server execution.

---

## Quickstart

### 1. Build from Source

Ensure you have Rust (edition 2024 / 1.85+) installed:

```bash
git clone https://github.com/khoipm/dragoman.git
cd dragoman
cargo build --release
```

The compiled binary will be placed at `./target/release/dragoman`.

### 2. One-Time Authentication (`login`)

Run the login command to authenticate with your Google AI Pro account:

```bash
./target/release/dragoman login
```

1. Chrome will launch automatically with remote debugging.
2. Navigate to [gemini.google.com](https://gemini.google.com) and log in with your Google account.
3. Once logged in, Dragoman automatically extracts the session tokens and saves them to `~/.config/dragoman/session.json`.
4. Close Chrome. You are now fully authenticated.

### 3. Verify Direct RPC Connection

Test the direct HTTP connection to Gemini 4 Argon without launching any browser:

```bash
./target/release/dragoman test-rpc "Say hello and confirm you are Gemini 4 Argon."
```

### 4. Start the Standalone Proxy Server

Launch the OpenAI-compatible proxy server:

```bash
./target/release/dragoman serve --port 8080
```

By default, the server listens on `http://127.0.0.1:8080`.

---

## Agent Configuration

Configure your external AI coding tool to use Dragoman:

| Setting | Value |
| :--- | :--- |
| **API Base URL** | `http://127.0.0.1:8080/v1` |
| **Model** | `gemini-4-argon` |
| **API Key** | `dragoman-local` (any arbitrary string) |

### Example: `curl` Non-Streaming

```bash
curl http://127.0.0.1:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gemini-4-argon",
    "messages": [
      {"role": "user", "content": "Explain the role of a dragoman in 1 sentence."}
    ],
    "stream": false
  }'
```

### Example: `curl` Streaming (SSE)

```bash
curl -N http://127.0.0.1:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gemini-4-argon",
    "messages": [
      {"role": "user", "content": "Write a Rust function that checks for prime numbers."}
    ],
    "stream": true
  }'
```

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

---

## Project Structure

```text
dragoman/
├── Cargo.toml          # Rust crate manifest
├── LICENSE             # MIT License
├── README.md           # Documentation
└── src/
    ├── main.rs         # CLI entry point (serve, login, test-rpc)
    ├── browser/        # Chrome launcher & CDP client (used only during login)
    │   ├── cdp.rs
    │   ├── launcher.rs
    │   └── mod.rs
    ├── openai/         # OpenAI API data structures and schemas
    │   ├── mod.rs
    │   └── types.rs
    ├── proxy/          # Axum HTTP proxy & agentic tool translator
    │   ├── mod.rs
    │   ├── server.rs
    │   └── translator.rs
    ├── session.rs      # Session credential persistence (~/.config/dragoman/session.json)
    └── web/            # Direct HTTP/2 StreamGenerate RPC client
        ├── client.rs
        └── mod.rs
```

---

## License

This project is licensed under the [MIT License](LICENSE).
