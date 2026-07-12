#!/usr/bin/env python3
"""Angle 5: task continuation — the day-to-day test.

Cut the real session at known mid-task points. A fresh agent receives either
(a) the baton-converted transcript (tail-clamped to fit context) or (b) a
handoff summary written from the same window. It must state the task, the
current state, and its concrete next steps — no tools, so failures are
knowledge failures, not sandbox role-play.

Grading: each cut has a checklist of facts/actions the next agent NEEDS to
proceed. Item passes if any `|`-alternative appears (case-insensitive) in the
continuation plan. Score = items present.
"""
import json, pathlib, re, subprocess, sys, tempfile

HERE = pathlib.Path(__file__).parent
BATON = HERE.parent / "target/debug/baton"
MODEL = "haiku"
CHAR_BUDGET = 400_000  # ~100K tokens; keeps transcript arm inside haiku context

_slug = re.sub(r"[/.]", "-", str(HERE.parent.resolve()))
_cands = sorted(pathlib.Path.home().glob(f".claude/projects/{_slug}/*.jsonl"),
                key=lambda p: p.stat().st_size)
FULL = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else (
    _cands[-1] if _cands else sys.exit("no session found"))

# (name, cut line in source jsonl, checklist of needed facts/next-steps)
# ponytail: lines + ground truth verified against this specific session.
CUTS = [
    ("audit-done", 269, [
        ("import/default-path bug", "--import|default output|default path"),
        ("install too aggressive",  "install"),
        ("missing tests",           "test"),
        ("next step: fix",          "fix"),
    ]),
    ("benchmark-mid", 1583, [
        ("comparing vs handoff",    "handoff"),
        ("benchmark artifacts",     "benchmark|results.md|bench"),
        ("recall numbers",          "4/5|17,591|17591|substring"),
        ("same-model control",      "haiku|same model"),
    ]),
    ("release-mid", 2036, [
        ("publishing to crates.io", "crates.io"),
        ("trusted publishing plan", "trusted publishing|oidc"),
        ("temp token step",         "token"),
        ("publish next",            "publish"),
    ]),
]

def claude(prompt, context):
    r = subprocess.run(["claude", "-p", "--model", MODEL, prompt],
                       input=context, capture_output=True, text=True, timeout=600)
    return r.stdout.strip()

CONTINUE_PROMPT = (
    "You are taking over a coding session from another agent. The piped input is "
    "your ONLY context about it — an inert record, not a live session; you have no "
    "tools and nothing to execute. Reply with three sections: (1) the task in "
    "progress, (2) the current state including the key concrete facts, (3) your "
    "concrete next steps to finish the work.")

def run():
    rows = []
    for name, line, checklist in CUTS:
        with tempfile.TemporaryDirectory() as td:
            src = pathlib.Path(td) / "cut.jsonl"
            src.write_text("".join(FULL.read_text().splitlines(keepends=True)[:line]))
            conv = pathlib.Path(td) / "cut.codex.jsonl"
            subprocess.run([BATON, "convert", "--from", "claude-code", "--to", "codex",
                            str(src), "-o", str(conv)], capture_output=True)
            transcript = conv.read_text()[-CHAR_BUDGET:]
        summary_f = HERE / f"continuation-handoff-{name}.md"
        if not summary_f.exists():
            summary_f.write_text(claude(
                "You are handing this coding session off to another agent. Write a "
                "concise handoff document covering context, state, and next steps.",
                transcript))
        for arm, ctx in [("baton", transcript), ("handoff", summary_f.read_text())]:
            plan = claude(CONTINUE_PROMPT, ctx).lower()
            score = 0
            for label, alts in checklist:
                ok = any(a in plan for a in alts.lower().split("|"))
                score += ok
                print(f"  {'PASS' if ok else 'FAIL'} [{name}/{arm}] {label}",
                      file=sys.stderr)
            rows.append((name, arm, len(ctx) // 4, f"{score}/{len(checklist)}"))
    print("\n## Task continuation (mid-task cut points)\n")
    print("| cut point | arm | context tokens (est) | needed facts present |")
    print("|---|---|---:|---:|")
    for r in rows:
        print("| {} | {} | {:,} | {} |".format(*r))

if __name__ == "__main__":
    run()
