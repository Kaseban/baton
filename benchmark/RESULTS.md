# baton benchmark

Two questions, measured from several angles:

1. **Mechanical fidelity** — when baton converts a session, what survives?
2. **Practical value** — does carrying the full transcript actually beat the
   common alternative (writing a handoff summary for the next agent)?

All runs use a real Claude Code session from developing baton itself
(3,375 KB, 896 messages, dozens of tool calls). Both arms of every recall
test use the same model (haiku) — only the context differs.

## Angle 1: Mechanical fidelity

Full session converted to every writable target, then round-tripped back to
claude-code.

| target | messages out | round-trip back to claude-code |
|---|---:|---:|
| claude-code | 896 | 896 |
| codex | 896 | 736 |
| gemini-cli | 896 | 723 |
| opencode | 896 | 584 |
| zed | 896 | 411 |
| aider | 896 | 111 |

Claude Code → Claude Code is lossless. Round-trip loss through other formats
is a property of the *target* format, not the converter: every message is
written out (896/896), but formats that don't model system/meta messages,
reasoning blocks, or per-tool-call records can't represent them, so they
collapse or drop on the way back. Aider is the extreme case — it stores only
chat text, so tool calls don't survive a round trip through it.

## Angle 2: Detail recall — baton transcript vs handoff summary

A fresh agent receives either (a) the baton-converted transcript or (b) a
handoff summary document written by an agent that saw the same transcript.
It then answers detail questions about the session (grading: expected
substring in answer).

| session size | arm | context tokens (est) | recall |
|---|---|---:|---:|
| sm (93 KB) | baton | 6,149 | **3/3** |
| sm | handoff | 440 | 1/3 |
| md (198 KB) | baton | 17,591 | **6/6** |
| md | handoff | 431 | 1/6 |
| lg (599 KB) | baton | 44,515 | **5/8** |
| lg | handoff | 1,222 | 1/8 |
| **total** | **baton** | | **14/17** |
| | handoff | | 3/17 |

## Angle 3: Scaling

The summary doesn't degrade gracefully — it degrades *immediately*. Even on
the small slice it lost concrete facts (line counts, versions, MSRV), because
a summary writer can't know which details the next agent will need. Recall
from the summary is flat (~1 per size) while the transcript stays high as the
session grows.

## Angle 4: Token cost

The transcript costs 10–40× more context tokens than the summary. That's the
honest trade: baton spends tokens to preserve facts; a summary saves tokens
by discarding them — and the receiving agent then re-reads files and re-runs
commands to rediscover what was discarded, which is the work the summary was
supposed to save.

## Caveats

- lg/baton's 3 misses: two answers where the model role-played needing file
  permissions despite the "inert record" instruction, and one mid-stream API
  stall — harness noise, not conversion loss (the facts are present in the
  transcript).
- Grading is substring match against hand-picked ground truth; regenerate the
  slices and expected answers if the source session changes.
- Single session, single model — directional, not a paper.

## Reproduce

```sh
cargo build
python3 benchmark/bench.py   # needs `claude` CLI on PATH
```

`bench.py` regenerates the fidelity matrix mechanically and re-runs the
recall quiz (the recall arm makes ~40 LLM calls).
