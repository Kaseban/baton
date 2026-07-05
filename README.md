<div align="center">

# 🏃 baton

**Pass the baton between coding agents.**

Convert any coding-agent session to any other. Wire itself into every agent's MCP config.

</div>

---

## Why

Every coding agent (Claude Code, OpenCode, Codex, Cursor, ...) stores sessions in its own format. When you switch agents — or want to resume a Claude session in OpenCode — there's no way to carry your conversation history with you. You start from scratch.

**baton fixes that.** One command converts sessions between formats. One command registers itself as an MCP server in every agent you have installed.

## Install

```sh
# Cargo (from source)
cargo install baton-mcp

# Cargo binstall (prebuilt binary)
cargo binstall baton-mcp

# Homebrew
brew install ehsanjso/tap/baton

# npm (downloads prebuilt binary, no Rust needed)
npx baton-mcp

# Or download a binary from GitHub Releases
```

## Quick start

```sh
# Convert a Claude Code session to OpenCode format
baton convert --from claude-code --to opencode \
  ~/.claude/projects/.../fa88b429-....jsonl \
  --import

# The --import flag runs `opencode import` automatically,
# so you can immediately resume:
opencode -s <imported-session-id>
```

```sh
# Register baton as an MCP server in every detected agent
baton install

# Verify
baton doctor

# Remove from all agents
baton uninstall
```

```sh
# List sessions across all agents
baton list

# List sessions from one agent
baton list --agent claude-code
```

## How it works

```
Claude Code session (.jsonl)
      │
      ▼
  baton read ──► canonical Session { messages: [Text, Reasoning, ToolCall, ToolResult] }
      │
      ▼
  baton write ──► OpenCode import JSON (SessionV1 schema)
```

Every agent format is read into a **canonical intermediate representation**, then written out in the target format. Adding a new format is O(1), not O(N×M) per-pair converters.

## Supported formats

| Agent | Read | Write | Auto-import |
|---|:---:|:---:|:---:|
| Claude Code | ✅ | ✅ | — |
| OpenCode | ✅ | ✅ | ✅ `opencode import` |
| Codex CLI | 🚧 | 🚧 | — |
| Cursor | 🚧 | — | — |
| Continue | 🚧 | 🚧 | — |
| Cline / Roo | 🚧 | 🚧 | — |
| Zed | 🚧 | 🚧 | — |
| Aider | 🚧 | 🚧 | — |
| Gemini CLI | 🚧 | 🚧 | — |

✅ = implemented · 🚧 = stubbed (format spec documented, implementation pending)

## MCP server

baton also runs as an MCP server, exposing four tools that any coding agent can call:

| Tool | Description |
|---|---|
| `list_sessions` | Scan all agents, return a unified list |
| `convert_session` | Convert a session from one format to another |
| `import_to_target` | Convert + run the target agent's import command |
| `detect_format` | Sniff a file/dir and report which agent produced it |

Run `baton install` to register itself in every agent's MCP config automatically.

## Building

```sh
git clone https://github.com/ehsanjso/baton.git
cd baton
cargo build --release
./target/release/baton --help
```

## Contributing

Format readers are the most impactful contribution. Each format lives in `src/formats/<name>.rs` and implements the `Format` trait (read + write). See `src/formats/claude_code.rs` for a complete reference implementation.

PRs welcome for any of the stubbed formats (Codex, Cursor, Continue, Cline, Zed, Aider, Gemini CLI).

## License

Dual-licensed under MIT OR Apache-2.0.
