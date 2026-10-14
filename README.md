# Dragoman

> **Dragoman** (*from Ottoman Turkish / Arabic dragoman: an official interpreter and diplomatic guide*): A high-performance, local OpenAI-compatible proxy written in Rust that bridges external coding agents (Cline, Aider, OpenHands, Codex) directly to **Gemini Web (`gemini.google.com`)** to access the **Gemini 4 Argon** reasoning model using your **Google AI Pro** account.

---

## Architecture Overview

Dragoman follows the **Embedded Browser & CDP Bridge** pattern (pioneered by tools like `codex-chatgpt-web`):

```text
External Coding Agent (Cline / Aider / OpenHands)
  │
  │ HTTP POST /v1/chat/completions (OpenAI JSON + Tools)
  ▼
Dragoman Proxy Server (Rust / Axum on http://127.0.0.1:8080)
  │
  ├── 1. PromptTranslator: Maps stateless turns & Tool JSON Schemas into Gemini prompts
  ├── 2. CdpClient: WebSocket communication via Chrome DevTools Protocol (CDP)
  │
  ▼
Dedicated Chrome Session (Port 9222, Persistent Profile)
  │
  ▼
Gemini Web UI (gemini.google.com with Google AI Pro session)
  │
  ├── Selects and runs: Gemini 4 Argon
  │
  ▼
Token Streaming & Tool Extraction
  │
  └── Emits standard OpenAI SSE chunks (`data: {"choices": [{"delta": ...}]}`) back to agent
```

---

## Features

- **Direct Gemini 4 Argon Access**: Routes prompts through your active Google AI Pro web subscription.
- **Zero API Billing / Key Required**: Does not consume Gemini API credits or require Google Cloud billing.
- **Full OpenAI Compatibility**: Exposes standard `/v1/chat/completions` and `/v1/models` endpoints.
- **Streaming & SSE Support**: Delivers tokens in real time via Server-Sent Events.
- **Agentic Tool Calling Emulation**: Injects function schemas into system instructions and parses `<tool_call>` blocks into OpenAI `tool_calls` structures.
- **Zero-Dependency Production Binary**: High-performance Rust binary with no heavy runtime dependencies.

---

## Quickstart

### 1. Build the Binary
```bash
cargo build --release
```
The binary will be generated at `./target/release/dragoman` (or run directly with `cargo run --`).

### 2. Log in with your Google AI Pro Account
Run the launcher once to open the dedicated browser profile:
```bash
./target/release/dragoman launch
```
In the browser window that opens:
1. Navigate to [gemini.google.com](https://gemini.google.com).
2. Log in with your Google account (`your-account@gmail.com`).
3. Verify that your Gemini 4 Argon model is active.
4. Your login session is saved in `~/.config/dragoman/chrome_profile` for future runs.

### 3. Check Status
Verify that the browser CDP interface and Gemini tab are connected:
```bash
./target/release/dragoman status
```

### 4. Start the Proxy Server
```bash
./target/release/dragoman serve --port 8080
```

---

## Configuring External Coding Agents

Point any OpenAI-compatible coding tool to Dragoman:

| Setting | Value |
| :--- | :--- |
| **API Base URL** | `http://127.0.0.1:8080/v1` |
| **Model** | `gemini-4-argon` |
| **API Key** | `dragoman-local` (any dummy value) |

### Example with `curl`
```bash
curl http://127.0.0.1:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gemini-4-argon",
    "messages": [{"role": "user", "content": "Write a quick Rust hello-world"}],
    "stream": true
  }'
```

---

## Repository Structure

- `src/`: Production Rust implementation.
  - `src/browser/`: Chrome launcher and CDP WebSocket client.
  - `src/openai/`: OpenAI API type definitions and SSE schemas.
  - `src/proxy/`: Axum HTTP server and prompt/tool translation engine.
  - `src/main.rs`: Command-line interface (`serve`, `launch`, `status`).
- `python/`: Python prototyping and inspection environment.
