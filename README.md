# gladiator

A terminal-based coding agent harness, written in Rust.

Gladiator connects an OpenAI-compatible LLM to a set of tools (shell, file
edits, grep, glob, web fetch, and external MCP servers) and drives it in an
agentic loop: read a task, plan with a todo list, call tools in parallel,
verify the result, and iterate. It ships with a ratatui TUI for interactive
use and an HTTP debug server for introspection.

> Built for, and largely by, the GLM-5.2 model — but it speaks the standard
> OpenAI Chat Completions streaming protocol and works against any compatible
> endpoint.

## Features

- **Agentic loop** — multi-turn tool-calling with parallel tool dispatch,
  per-tool result coalescing, and an iteration cap.
- **Built-in tools** — `bash`, `read_file`, `write_file`, `edit_file`,
  `glob`, `grep`, `web_fetch`.
- **MCP integration** — spawn external tool servers (Model Context Protocol)
  as child processes and expose their tools to the agent. Runtime admin tools
  let the agent inspect, restart, and disable misbehaving servers.
- **Subagents** — `call_subagent` spawns a nested agent with its own fresh
  context, system prompt, and tool budget. Output is indented in the TUI;
  pending user messages are delivered to the active subagent for steering.
- **Context refresh** — when the context window fills, the agent writes a
  self-contained handoff note and calls `restart_from_file` to shed history
  and continue.
- **Loop detection** — a similarity-based stream monitor breaks think-loops
  and idle streams; a stuck-model triage path injects guidance to recover.
- **Five-whys analysis** — surprises (max-iter, stuck loops) are recorded
  for root-cause analysis on the next context refresh.
- **Persistence** — save/restore conversation state to disk; resume a
  session from a file.
- **TUI** — streaming chat with diff rendering for edits, pending-message
  queue, slash commands (`/save`, `/load`, `/fixme`, `/mcp`, ...), and live
  token-usage + ETA in the status bar.
- **HTTP debug server** — bus/topic introspection and SSE streams for
  out-of-process observation.

## Workspace layout

| Crate | Purpose |
|---|---|
| `gladiator` (root bin) | Entry point: wires the bus, actors, tool registry, and MCP servers; runs the TUI or headless. |
| `gladiator-core` | Core primitives: pub/sub `Bus`, `Actor` trait, `Message`, TOML config. |
| `gladiator-llm` | LLM actor: OpenAI-compatible streaming client, SSE framing, loop detector. |
| `gladiator-tools` | Tool registry, built-in tools, MCP client + admin tools. |
| `gladiator-agent` | The agent actor loop, conversation state, subagent frames, internal tools, persistence, five-whys. |
| `gladiator-tui` | Ratatui + crossterm terminal UI, slash commands, diff rendering. |
| `gladiator-server` | Axum HTTP debug server with bus introspection and SSE streams. |
| `mcp-*` | Standalone MCP server crates (random, rlsp/rust-analyzer bridge, websearch, pty, pdf, tagged-fileops, loader, test-client). |

## Getting started

### Prerequisites

- Rust (edition 2021/2024) — install via [rustup](https://rustup.rs).
- An OpenAI-compatible LLM endpoint reachable over HTTP.

### Build & run

```bash
cargo build --release
cargo run --release            # TUI mode (default)
cargo run --release -- --no-tui  # headless mode
```

CLI flags:

```
gladiator [OPTIONS]

Options:
  -c, --config <PATH>  Config file path (default: ./gladiator.toml if present)
      --host <HOST>    HTTP debug server host (default: 127.0.0.1)
      --port <PORT>    HTTP debug server port (default: 3000)
      --no-tui         Disable the TUI (run headless)
  -h, --help           Print help
```

### Configuration

Gladiator looks for `gladiator.toml` in the working directory (or the path
passed to `--config`). All fields have sensible defaults; override as needed.

```toml
[llm]
model = "custom/glm-5.2"
base_url = "http://localhost:4000/v1"
api_key = ""
temperature = 0.2
max_tokens = 65536

[agent]
max_iterations = 200
working_dir = "."
# Prefix with "@" to load from a file, e.g. "@misc/system.txt"
system_message = "You are gladiator, an autonomous coding agent..."

[tools]
bash = true
read = true
write = true
edit = true
glob = true
grep = true
web_fetch = true

[tools.sandbox]
enabled = false
network = false

# External MCP tool servers. Each entry spawns a child process.
[mcp_servers.my-server]
command = ["target/release/my-server"]
default = true            # start on launch
expose = []               # empty = expose all tools; list names to filter
env = { FOO = "bar" }
```

If the system message starts with `@`, the remainder is treated as a path to
a file containing the real prompt (e.g. `@misc/system.txt`).

## Internal tools

These are handled inline against the agent's conversation state (no external
tool runner):

- **`todo_write` / `todo_read`** — transient per-agent todo list used for
  planning multi-step tasks. At most one `in_progress` item is enforced.
- **`restart_from_file`** — snapshot the current context to `/tmp`, wipe
  history, and inject fresh instructions read from a file. Used for context
  refresh via handoff notes.
- **`set_context_reminder`** — inject a message once usage crosses a token
  threshold (e.g. a nudge to refresh context).
- **`schedule_wake_up`** — one-shot or recurring cron-style wake-up that
  injects a message into the agent's pending queue at a future time.
- **`call_subagent`** — push a new subagent frame with a fresh context and
  system prompt; the inner agent runs to completion (text-only response)
  and its output becomes the tool result for the parent.

## Development

```bash
cargo build                          # build all crates
cargo test                           # unit + integration tests
cargo test --test e2e_test -- --ignored  # E2E (requires a live LLM endpoint)
cargo clippy                         # lint
cargo fmt                            # format
```

Logging goes to `gladiator.log` (TUI mode) or stderr (headless). Control
verbosity with `RUST_LOG=gladiator_agent=debug,gladiator_llm=info`.

## Architecture

```
                    ┌─────────┐   user input   ┌────────┐
                    │   TUI   │ ─────────────▶ │        │
                    │ (ratatui)│ ◀──────────── │        │
                    └────┬────┘   stream/draw  │        │
                         │                     │  Bus   │
                    ┌────▼────┐                │ (pub/  │
         SSE ◀──────│ debug   │                │  sub)  │
                    │ server  │                │        │
                    └────┬────┘                │        │
                         │                     │        │
            ┌────────────┼────────────┐        │        │
            ▼            ▼            ▼        │        │
       ┌─────────┐ ┌─────────┐ ┌──────────┐    │        │
       │  Agent  │ │   LLM   │ │  Tools   │◀───┘        │
       │  actor  │ │  actor  │ │ registry │             
       └────┬────┘ └────▲────┘ └────┬─────┘             
            │           │           │                   
            └─────chat completions (streaming)──────────┘
                        + tool_calls + tool_results
```

The bus decouples every actor: the agent publishes chat-completion requests,
the LLM actor streams tokens back; the agent dispatches tool calls, tool
runners publish results. The TUI and HTTP server subscribe to the topics
they care about and never touch the agent directly.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option. Contributions intentionally submitted for inclusion in this
project by you, as defined in the Apache-2.0 license, shall be dual licensed
as above, without any additional terms or conditions.
