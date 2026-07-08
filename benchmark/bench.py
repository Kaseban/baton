#!/usr/bin/env python3
"""Benchmark: baton (lossless conversion) vs handoff-summary for cross-agent session transfer.

Arm A: target agent gets the baton-converted full transcript.
Arm B: target agent gets a handoff summary doc (what /handoff-style skills produce).
Both answer the same detail questions about the session; graded by substring match.
"""
import json, subprocess, sys, pathlib

HERE = pathlib.Path(__file__).parent
CONVERTED = HERE / "converted-codex.jsonl"
SUMMARY = HERE / "handoff-summary.md"
MODEL = "haiku"  # same model both arms; only the context differs

# ponytail: ground truth hand-picked from this specific slice; regenerate if slice changes
QUESTIONS = [
    ("What version is set in Cargo.toml of this project?", "0.1.0"),
    ("What Rust edition does the crate use?", "2024"),
    ("How many lines long is src/formats/claude_code.rs?", "355"),
    ("What rust-version (MSRV) is set in Cargo.toml?", "1.88"),
    ("What did the user originally ask for at the start of the session?", "improvement"),
]

def claude(prompt: str, context: str) -> str:
    r = subprocess.run(
        ["claude", "-p", "--model", MODEL, prompt],
        input=context, capture_output=True, text=True, timeout=300,
    )
    return r.stdout.strip()

def main():
    transcript = CONVERTED.read_text()

    if not SUMMARY.exists():  # generate handoff doc once
        SUMMARY.write_text(claude(
            "You are handing this coding session off to another agent. "
            "Write a concise handoff document covering context, state, and next steps.",
            transcript))
    summary = SUMMARY.read_text()

    arms = {"baton (full transcript)": transcript, "handoff (summary doc)": summary}
    print(f"{'arm':<26} {'context tokens (est)':>20} {'accuracy':>10}")
    for name, ctx in arms.items():
        score = 0
        for q, expect in QUESTIONS:
            ans = claude(
                "Answer from the session context above only. Question: " + q, ctx)
            ok = expect.lower() in ans.lower()
            score += ok
            print(f"  {'PASS' if ok else 'FAIL'} [{name.split()[0]}] {q[:60]} -> {ans[:80]!r}", file=sys.stderr)
        print(f"{name:<26} {len(ctx)//4:>20} {score}/{len(QUESTIONS):>9}")

if __name__ == "__main__":
    main()
