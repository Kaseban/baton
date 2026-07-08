# baton vs handoff-summary benchmark

Real Claude Code session slice (29 msgs, 12 tool calls) → transferred to a fresh agent two ways, then quizzed on 5 session details (grading: expected substring in answer, same haiku model both arms).

| arm | context tokens (est) | detail accuracy |
|---|---:|---:|
| baton (lossless convert) | 17,591 | 4/5 |
| handoff summary doc | 637 | 0/5 |

Summary handoff lost every specific fact (crate version, edition, MSRV, line counts); the receiving agent had to re-read files — re-doing the work the summary "saved". baton's converted transcript answered from context alone. Mechanical fidelity: 12/12 tool calls, all 29 messages preserved in conversion.

Reproduce: `python3 benchmark/bench.py` (needs `claude` CLI; regenerate slice + ground truth if you change the source session).
