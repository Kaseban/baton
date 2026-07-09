# Launch posts — copy-paste ready

## 1. Show HN

**Title** (80 char limit, no superlatives — HN strips/penalizes them):

> Show HN: Baton – Convert coding-agent sessions between Claude Code, Codex, etc.

**URL:** https://github.com/Kaseban/baton

**Text:**

I kept hitting the same wall: three hours into a Claude Code session, usage limit hits, and the session resets at 10pm. All the context — architecture decisions, edge cases we'd mapped, half-written diff — was locked in a format only Claude Code could read.

So I built baton. It converts coding-agent sessions between formats: Claude Code, OpenCode, Codex CLI, Gemini CLI, Zed, Aider, Cursor, Continue, and Cline/Roo. One command, and you keep going in a different agent:

    baton convert --from claude-code --to opencode session.jsonl --import

Everything goes through a canonical intermediate representation (text / reasoning / tool-call / tool-result messages), so adding a format is one reader + one writer, not N×M pair converters. Written in Rust; also runs as an MCP server so an agent can pass the baton itself mid-conversation.

Before shipping I benchmarked whether carrying the full transcript actually beats writing a handoff summary for the next agent (same model both arms, only context differs, real 3.4MB session). Transcript recalled 14/17 concrete facts; summary recalled 3/17. The summary lost versions, line counts, MSRV — even on a 93KB slice. Full methodology and caveats in benchmark/RESULTS.md.

Honest limitations: Cursor and Cline are read-only (their state lives in editor SQLite/globalState with no import path), and Aider's format only stores chat text, so round-trips through it are lossy by design.

Install: npm/brew/cargo/shell — prebuilt binaries, no Rust needed for npm.

Would love feedback, especially from anyone who's tried other approaches to cross-agent session portability.

---

## 2. X/Twitter thread

**Tweet 1** (attach assets/demo.gif):

It's 4pm. Claude Code: "usage limit reached — resets at 10pm."

Three hours of context. Architecture decided. Half the diff written. Gone.

I built baton to fix this: pass your session to any other coding agent and keep going.

github.com/Kaseban/baton

**Tweet 2:**

One command:

baton convert --from claude-code --to opencode session.jsonl --import

9 agents supported: Claude Code, OpenCode, Codex CLI, Gemini CLI, Zed, Aider, Cursor, Continue, Cline/Roo.

**Tweet 3:**

"Why not just write a handoff summary?"

I benchmarked it. Same model, same task, only context differs:

Full transcript: 14/17 facts recalled
Handoff summary: 3/17

Summaries lose the details that matter — versions, line counts, decisions.

**Tweet 4:**

It's also an MCP server — your agent can pass the baton itself, mid-conversation.

Rust, canonical IR (adding formats is O(1) not O(N×M)), prebuilt binaries via npm/brew/cargo.

Open source, MIT/Apache-2.0. Don't drop the baton. 🏃

---

## 3. r/ClaudeAI

**Title:**

> Hit your Claude Code usage limit mid-session? I built a tool that converts the session to OpenCode/Codex/Gemini so you can keep going

**Body:**

Every Claude Code power user knows the 4pm wall: "usage limit reached, resets at 10pm" — right in the middle of a deep session.

baton converts your Claude Code session (the .jsonl in ~/.claude/projects/) into any of 8 other agent formats. With OpenCode it even auto-imports:

    baton convert --from claude-code --to opencode session.jsonl --import
    opencode -s <session-id>

Same conversation, different runner. When your Claude limit resets, convert back.

I benchmarked full-transcript passing vs writing a handoff summary — transcript kept 14/17 concrete facts vs 3/17 for the summary (details in the repo).

Also works as an MCP server, so Claude Code itself can list/convert sessions as tools (`baton install` registers it in every detected agent's MCP config).

Free, open source, Rust: https://github.com/Kaseban/baton

Feedback welcome — especially which direction you'd convert most, it helps prioritize writers.

---

## Posting notes

- **Order**: HN first (morning US East, Tue–Thu best), X thread ~1h later linking HN discussion, Reddit same day.
- **HN**: post URL + text together. Reply to every comment fast in first 2h — engagement drives ranking.
- **Reddit**: r/ClaudeAI allows self-promo with substance; lead with the pain, not the tool.
- **Do NOT** post to r/programming day 1 — link-only culture, needs traction first.
- Set social preview image before posting (link cards).
