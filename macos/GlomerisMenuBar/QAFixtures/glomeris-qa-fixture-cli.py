#!/usr/bin/env python3
"""DEV-QA fixture stand-in for the `glomeris` CLI (HORO-2176).

NOT part of the app bundle and never shipped. A DEBUG build of GlomerisMenuBar
spawns this instead of the real CLI only when GLOMERIS_QA_FIXTURE_CLI points at
it (see Sources/GlomerisQAFixtures.swift), so every screen is driven by the
real DTO decoding and the real SwiftUI code over a deterministic result set.

Safety properties, each asserted by Tests/GlomerisQAFixtureCliTests.swift:
  * It reads nothing from the machine: no filesystem walk, no Docker, no git.
    Its only file access is the optional state directory named by
    GLOMERIS_QA_STATE_DIR (an invocation log, and a list of dropped ids).
  * It has no cleanup authority. `execute` and every other subcommand except
    the read-only ones below exit 64 and are logged as REFUSED.
  * Every resource id starts with `devqa-synthetic:` so a synthetic row cannot
    be mistaken for, or collide with, a real resource.

Environment:
  GLOMERIS_QA_COUNT       number of synthetic candidates (default 0)
  GLOMERIS_QA_STATE_DIR   directory for invocations.log and dropped.txt
"""
import json
import os
import sys

KINDS = ["docker_image", "docker_volume", "cargo_target_dir", "node_modules"]
# 10-slot cycle: every class appears at every scale >= 10, and PROTECTED /
# UNKNOWN_INCOMPLETE (the classes that must never be hidden) are the majority.
LABELS = [
    "AUTO_SAFE",
    "ASK", "ASK",
    "PROTECTED", "PROTECTED", "PROTECTED", "PROTECTED",
    "UNKNOWN_INCOMPLETE", "UNKNOWN_INCOMPLETE", "UNKNOWN_INCOMPLETE",
]
READ_ONLY = {"detect", "explain", "--version", "version"}


def state_dir():
    return os.environ.get("GLOMERIS_QA_STATE_DIR") or None


def log(line):
    d = state_dir()
    if d:
        with open(os.path.join(d, "invocations.log"), "a") as fh:
            fh.write(line + "\n")


def dropped():
    d = state_dir()
    if not d:
        return set()
    try:
        with open(os.path.join(d, "dropped.txt")) as fh:
            return {ln.strip() for ln in fh if ln.strip()}
    except OSError:
        return set()


def resource_id(i):
    kind = KINDS[i % len(KINDS)]
    base = "devqa-synthetic:%s:%05d" % (kind, i)
    if i % 50 == 49:
        # A long, path-like identifier, to exercise middle truncation.
        base += ":/Users/devqa/Projects/" + "/".join(["very-long-segment-%02d" % n for n in range(12)])
    return base


def candidate(i):
    label = LABELS[i % len(LABELS)]
    actionable = label in ("AUTO_SAFE", "ASK")
    size = 1_048_576 * (1 + (i * 7) % 900)
    tier = "large" if size > 600 * 1_048_576 else "notable" if size > 200 * 1_048_576 else "normal"
    return {
        "resource_id": resource_id(i),
        "kind": KINDS[i % len(KINDS)],
        "reclaimable_bytes": size,
        "reclaimable_human": "%d MB" % (size // 1_048_576),
        "reclaimable_bytes_is_lower_bound": label == "UNKNOWN_INCOMPLETE",
        "impact_tier": tier,
        "policy_label": label,
        "reasons": {
            "AUTO_SAFE": ["no_active_use_observed"],
            "ASK": ["resource_in_active_use"],
            "PROTECTED": ["protected_executable_dependency"],
            "UNKNOWN_INCOMPLETE": ["evidence_incomplete"],
        }[label],
        "executable": actionable,
        "offered_actions": (
            [{"action_id": "devqa.fixture.clean", "requires_confirmation": label == "ASK"}]
            if actionable else []
        ),
        "refusal_reason": None if actionable else (
            "PROTECTED: protected_executable_dependency" if label == "PROTECTED"
            else "no registered cleanup action for this resource kind"
        ),
    }


def visible_indices():
    n = int(os.environ.get("GLOMERIS_QA_COUNT", "0") or 0)
    gone = dropped()
    return [i for i in range(n) if resource_id(i) not in gone]


def detect():
    cands = [candidate(i) for i in visible_indices()]
    return {
        "candidates": cands,
        "detectors": [{"detector": "devqa_fixture", "status": "found", "candidates_found": len(cands)}],
        "discovery_complete": True,
        "eligibility": "eligible" if any(c["executable"] for c in cands) else "nothing_eligible",
        "correlation_complete": True,
        "uncorrelated_candidates": 0,
    }


def explain(rid):
    for i in visible_indices():
        if resource_id(i) == rid:
            c = candidate(i)
            protected = c["policy_label"] == "PROTECTED"
            return {
                "resource_id": c["resource_id"],
                "kind": c["kind"],
                "detector": "devqa_fixture",
                "sources": [],
                "logical_bytes": c["reclaimable_bytes"],
                "logical_human": c["reclaimable_human"],
                "reclaimable_bytes": c["reclaimable_bytes"],
                "reclaimable_human": c["reclaimable_human"],
                "reclaimable_bytes_is_lower_bound": c["reclaimable_bytes_is_lower_bound"],
                "completeness": "incomplete" if c["policy_label"] == "UNKNOWN_INCOMPLETE" else "complete",
                "confidence": "high",
                "active_use_signals": [],
                "regenerability": "regenerable_by_rebuild",
                "policy_label": c["policy_label"],
                "reasons": c["reasons"],
                "native_cleanup_available": False,
                "native_cleanup_action_id": None,
                "fingerprint_token": None,
                "executable": c["executable"],
                "offered_actions": c["offered_actions"],
                "refusal_reason": c["refusal_reason"],
                "refusal_source": "policy_protected" if protected else None,
                "holders": [],
                "holder_probes": [{"relation": "open_file"}, {"relation": "cwd"}, {"relation": "executable"}],
                "executable_dependency": {
                    "probe": "observed", "references": [], "unresolved": [], "sources_examined": [],
                    "note": "DEV-QA synthetic data: nothing was examined",
                },
                "git": None,
            }
    return None


def subcommand(args):
    skip = False
    for a in args:
        if skip:
            skip = False
            continue
        if a == "--project-root":
            skip = True
            continue
        if a.startswith("--") and a not in READ_ONLY:
            continue
        return a
    return ""


def main(argv):
    args = argv[1:]
    sub = subcommand(args)
    if sub in ("--version", "version") or "--version" in args:
        log("version")
        print("glomeris 0.0.0-devqa-fixture")
        return 0
    if sub == "detect":
        log("detect")
        json.dump(detect(), sys.stdout)
        return 0
    if sub == "explain":
        rest, skip = [], False
        for a in args:
            if skip:
                skip = False
            elif a == "--project-root":
                skip = True
            elif a != "explain" and not a.startswith("--"):
                rest.append(a)
        rid = rest[-1] if rest else ""
        log("explain")
        report = explain(rid)
        if report is None:
            sys.stderr.write("DEV-QA fixture CLI: unknown or dropped resource\n")
            return 1
        json.dump(report, sys.stdout)
        return 0
    log("REFUSED %s" % (sub or "<none>"))
    sys.stderr.write("DEV-QA fixture CLI: '%s' is not supported; this stand-in has no cleanup authority\n" % sub)
    return 64


if __name__ == "__main__":
    sys.exit(main(sys.argv))
