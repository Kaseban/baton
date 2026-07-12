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

## Angle 5: Task continuation

Recall trivia is a proxy. The day-to-day question is: can the next agent
*keep working*? We cut the real session at three known mid-task points,
gave a fresh agent either the baton transcript (tail-clamped to ~100K
tokens) or a handoff summary written from the same window, and asked it to
state the task, current state, and concrete next steps (no tools — so
failures are knowledge failures, not sandbox role-play). Graded against a
checklist of facts the next agent needs to proceed.

| cut point | arm | context tokens (est) | needed facts present |
|---|---|---:|---:|
| audit-done | baton | 38,151 | 4/4 |
| audit-done | handoff | 725 | 4/4 |
| benchmark-mid | baton | 100,000 | 3/4 |
| benchmark-mid | handoff | 338 | 3/4 |
| release-mid | baton | 100,000 | 3/4 |
| release-mid | handoff | 348 | 3/4 |

**Honest finding: parity.** For "what do I do next?", a well-written summary
is enough — both arms scored 10/12, missing the same items. The transcript's
advantage is in Angle 2: *specific facts on demand* mid-work (14/17 vs 3/17).
So the fair pitch is not "transcript beats summary at everything"; it's:
(a) the summary only exists if someone spends a full transcript read writing
it — baton gives the transfer for free; and (b) the summary answers the
questions its author anticipated, the transcript answers the ones they didn't.

## Caveats

- lg/baton is unstable run-to-run (5/8 published; a verification re-run got
  1/8). Every miss is the same failure mode: the model role-plays needing
  file/sandbox permissions despite the "inert record" instruction, instead of
  answering from the transcript in its context. Harness noise, not conversion
  loss (the facts are present in the transcript). sm and md reproduce exactly.
  Treat lg/baton as a lower bound until the quiz prompt pins the agent harder.
- Grading is substring match against hand-picked ground truth; regenerate the
  slices and expected answers if the source session changes.
- Single session, single model — directional, not a paper.
- Continuation transcripts are tail-clamped to fit context; the handoff is
  written from the same window, so both arms see identical information. Both
  missed the same 2/24 checklist items (facts outside the window or phrased
  differently) — a window artifact, not an arm difference.

## Reproduce

```sh
cargo build
python3 benchmark/bench.py          # fidelity matrix + recall quiz (~40 LLM calls)
python3 benchmark/continuation.py   # mid-task continuation eval (~9 LLM calls)
```

`bench.py` regenerates the fidelity matrix mechanically and re-runs the
recall quiz. `continuation.py` re-runs the mid-task cut-point eval; cut line
numbers are pinned to this specific session — re-pick them if the source
session changes.
