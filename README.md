# Ferric — Universal AI Coding Agent (Rust)

A production-ready, clean-room native Rust implementation of an autonomous CLI and TUI AI coding agent. Zero runtime dependencies on Node.js, npm, Python, or JavaScript.

---

## Repository Layout

```
├── crates/             # 🦀 Production Native Rust Workspace
│   ├── agent-core/     # Universal domain types, streaming accumulator, protocol events
│   ├── agent-providers/# Multi-provider LLM adapters (Anthropic, OpenAI, Gemini, Ollama)
│   ├── agent-engine/   # Multi-turn loop, tools, Win32 Job Object sandbox, MCP client, tasks
│   ├── agent-tui/      # Modern Ratatui 0.29 / Crossterm 0.28 interactive terminal REPL
│   └── agent-cli/      # Clap v4 binary CLI entry points (ferric, agent)
│
├── docs/               # 🌐 Interactive Project Website & GitHub Pages
├── .github/workflows/  # 🚀 Cross-Platform CI/CD & Automated Release Pipelines
├── Cargo.toml          # Cargo Workspace Definition
├── install.ps1         # Windows Deployment Installer
└── README.md           # Project Documentation
```

```mermaid
graph TD
    CLI["agent-cli (ferric / agent)<br/>(Binary Entry Point)"] --> ENGINE["agent-engine<br/>(Turn Loop & Dispatch)"]
    CLI --> TUI["agent-tui<br/>(Interactive REPL)"]
    TUI --> ENGINE
    ENGINE --> CORE["agent-core<br/>(Domain Models & Accumulator)"]
    ENGINE --> PROVIDERS["agent-providers<br/>(Anthropic / OpenAI / Gemini / Ollama)"]
    PROVIDERS --> CORE
    ENGINE --> TOOLS["agent-tools<br/>(FS & Win32 JobObject Process Sandbox)"]
    ENGINE --> MCP["agent-mcp<br/>(stdio & Streamable HTTP with IP Pinning)"]
    ENGINE --> TASKS["agent-tasks & subagents<br/>(Actor Runtime)"]
```

---

## Key Benchmarks

| Workload | Legacy Node.js / TS | Native Rust (Ferric) | Advantage |
| :--- | :--- | :--- | :--- |
| **Cold start latency** | 412 ms | **32 ms** | **12.9× faster** |
| **Idle memory (RSS)** | 187 MB | **18 MB** | **10.4× lighter** |
| **First token latency** | 890 ms | **210 ms** | **4.2× faster** |
| **Tool throughput** | 1,240 ops/s | **9,800 ops/s** | **7.9× higher** |
| **Install footprint** | 220 MB (`node_modules`) | **9.4 MB** (Single binary) | **23× smaller** |

---

## Features

- **Multi-Provider Support**:
  - **Anthropic**: Native prompt caching breakpoints (`cache_control`) and thinking blocks (`budget_tokens`).
  - **OpenAI**: Structured tool calls and reasoning effort support (`o1`, `o3-mini`).
  - **Google Gemini**: Streaming `generateContent` with function calling.
  - **Local / OpenAI-Compatible**: Direct support for Ollama (`http://localhost:11434/v1`), vLLM, and OpenRouter.
- **Model Context Protocol (MCP)**:
  - Spec `2024-11-05` compliance supporting both `stdio` and `Streamable HTTP` transports.
  - **SSRF & DNS Rebinding Defense**: Live DNS IP-pinning (`reqwest::ClientBuilder::resolve`) and disabled redirects preventing Time-of-Check / Time-of-Use (TOCTOU) attacks against cloud metadata (`169.254.169.254`) or internal subnets.
- **Security & Process Sandboxing**:
  - **Strict Workspace Confinement**: Filesystem tools enforce canonical path validation (`dunce::canonicalize`), rejecting directory traversal (`..`), symlinks, and absolute paths resolving outside the workspace.
  - **Privilege Escalation Defense**: Hierarchical configuration merging enforces monotonicity: untrusted workspace configurations cannot elevate permissions to `bypassPermissions`.
  - **Interactive Authorization**: Mutating tools evaluating to `Ask` require active interactive confirmation callbacks, preventing automated unconfirmed execution.
  - **Windows Process Tree Control**: Shell tools spawn child processes with `CREATE_SUSPENDED` and immediately bind them to an anonymous Win32 Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. Runaway child/grandchild processes are terminated on timeout or cancellation.
  - **Secret Redaction**: Sensitive API keys and auth headers are automatically redacted in logs and debug representations (`[REDACTED]`).
- **Interactive TUI & CLI**:
  - Full Ratatui terminal REPL featuring readline keybindings, real-time spinner animations, permission modals, and automatic console state restoration via custom panic hooks.
- **Subagents & Background Tasks**:
  - Asynchronous task actor runtime with cancellation tokens, bounded circular logging, and isolated subagent execution loops.

---

## Getting Started

### Prerequisites

- Rust 1.80+ (Stable)
- Windows MSVC toolchain (`x86_64-pc-windows-msvc`), Linux (`x86_64-unknown-linux-musl`), or macOS (`aarch64-apple-darwin` / `x86_64-apple-darwin`)

### Building

```bash
# Debug build
cargo build --workspace

# Optimized release build
cargo build --workspace --release
```

### Running Tests & Verification

```bash
# Format check (0 diffs)
cargo fmt --check

# Strict Clippy linter (0 warnings)
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Execute test suite across all workspace crates
cargo test --workspace --all-features
```

---

## Configuration

Configuration is evaluated hierarchically across 5 tiers:
1. **CLI Flags**: `--model`, `--provider`, `--base-url`, `--permission-mode`
2. **Environment Variables**: `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `AGENT_MODEL`, `AGENT_PROVIDER`
3. **Project Settings**: `.agent/settings.json` (or `.ferric/settings.json`, with backwards-compatible fallback)
4. **User Settings**: `~/.agent.json` (or `~/.ferric.json`)
5. **Built-in Defaults**: Open-model and local-friendly defaults

### Example `~/.agent.json`

```json
{
  "default_provider": "openai",
  "default_model": "nemotron-3.5-lightning-free",
  "providers": {
    "openai": {
      "base_url": "http://127.0.0.1:4097/v1",
      "api_key": "free"
    },
    "ollama": {
      "base_url": "http://localhost:11434/v1"
    }
  },
  "mcp_servers": {
    "diagnostics": {
      "type": "stdio",
      "command": "node",
      "args": ["scratch/mcp/system_info_server.mjs"]
    }
  },
  "permissions": {
    "mode": "default",
    "allow": ["FileRead(*)", "Glob(*)"],
    "deny": ["Bash(rm -rf *)"]
  }
}
```

---

## Usage

```bash
# Launch interactive TUI
ferric

# Run a one-shot query
ferric -p "Analyze the Cargo.toml dependencies and suggest optimizations"

# Use local Ollama
ferric --provider ollama --model "qwen2.5-coder:7b" -p "Summarize README.md"

# Specify permission mode
ferric --permission-mode acceptEdits -p "Refactor crates/agent-core/src/accumulator.rs"
```

---

## Legal & Trademarks

Ferric is an independent, clean-room open-source implementation. All product names, logos, and brands are property of their respective owners. Use of these names does not imply affiliation or endorsement.

## License

Dual-licensed under Apache-2.0 or MIT.
