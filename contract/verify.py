#!/usr/bin/env python3
"""Contract suite runner (compare mode) — Wave R, R-1. Python 3 stdlib only.

Runs contract/cases/ against a server binary and diffs every response against
the committed golden files (framework-agnostic parity: the same check runs
against the Go server and the Rust server), re-checking each case's raw-body
`text` expectation when one is present. Then runs contract/deltas/ and
checks each delta against its `go_status`/`rust_status` (+ optional
`go_json`/`rust_json`/`go_text`/`rust_text`) for the selected --target.

Exit 1 with a full diff report on any mismatch.
"""
import argparse
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import capture

HERE = os.path.dirname(os.path.abspath(__file__))


def delta_expect(case, target):
    expect = {}
    status_key = f"{target}_status"
    if status_key not in case:
        raise RuntimeError(f"delta case {case['name']} has no {status_key}")
    expect["status"] = case[status_key]
    if f"{target}_json" in case:
        expect["json"] = case[f"{target}_json"]
    if f"{target}_text" in case:
        expect["text"] = case[f"{target}_text"]
    shared = case.get("expect", {})
    if "headers" in shared:
        expect["headers"] = shared["headers"]
    return expect


def parity_text_expect(case):
    """Re-check raw-body `text` expectations for parity cases at verify time.
    Golden comparison alone loses byte-exactness: golden bodies store parsed
    JSON when parseable, so quirks like the json.Encoder trailing newline or
    error-body spacing would not bind the Rust target without this. Capture
    applied the same normalization to the raw text, so the comparison rules
    are identical on both sides."""
    text = case.get("expect", {}).get("text")
    return {"text": text} if text is not None else {}


def compare_golden(actual, golden, where):
    if actual == golden:
        return []
    return [
        f"{where}: golden mismatch\n"
        f"  golden: {json.dumps(golden, sort_keys=True)}\n"
        f"  actual: {json.dumps(actual, sort_keys=True)}"
    ]


def main():
    ap = argparse.ArgumentParser(description="Verify a server binary against the contract suite.")
    ap.add_argument("--binary", required=True, help="path to the server binary under test")
    ap.add_argument("--port", type=int, default=18081)
    ap.add_argument("--target", choices=["go", "rust"], default="rust",
                    help="which side of the delta fixtures to assert (default rust)")
    ap.add_argument("--cases", default=os.path.join(HERE, "cases"))
    ap.add_argument("--deltas", default=os.path.join(HERE, "deltas"))
    ap.add_argument("--golden", default=os.path.join(HERE, "golden"))
    ap.add_argument("--seed", default=os.path.join(HERE, "seed.sql"))
    args = ap.parse_args()

    cases = capture.load_cases(args.cases)
    deltas = capture.load_cases(args.deltas)
    all_cases = cases + deltas
    stub = capture.OidcStub() if capture.needs_stub(all_cases) else None
    if stub:
        stub.start()

    failures = []
    checked = 0
    try:
        # Parity cases: compare against golden AND re-check any raw-body
        # `text` expectation (golden stores parsed JSON when parseable, so
        # byte-exact bodies only bind via the text re-check).
        for group in capture.group_cases(cases):
            results, diffs = capture.run_group(
                args.binary, args.port, group, args.seed, stub,
                expect_fn=parity_text_expect,
            )
            failures.extend(diffs)
            for name, actual in results.items():
                golden_path = os.path.join(args.golden, name + ".json")
                if not os.path.exists(golden_path):
                    failures.append(f"{name}: no golden file at {golden_path}")
                    continue
                with open(golden_path) as f:
                    golden = json.load(f)
                failures.extend(compare_golden(actual, golden, name))
                checked += 1

        # Delta cases: assert the target-specific behavior.
        for group in capture.group_cases(deltas):
            results, diffs = capture.run_group(
                args.binary, args.port, group, args.seed, stub,
                expect_fn=lambda c: delta_expect(c, args.target),
            )
            failures.extend(diffs)
            checked += len(results)
    finally:
        if stub:
            stub.stop()

    if failures:
        print(f"VERIFY FAILED ({args.target} target): {len(failures)} mismatch(es) in {checked} cases")
        for d in failures:
            print(" -", d)
        return 1
    print(f"verify ok ({args.target} target): {checked} cases green")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
