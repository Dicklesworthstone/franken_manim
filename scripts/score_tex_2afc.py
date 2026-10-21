#!/usr/bin/env python3
"""Score a blind 2AFC TeX review session (fm-tex-2afc-kit-b9k2).

The reviewer saw each pair once and picked the side they believed is LaTeX. This scores the
answers against the kit's key and applies the pass rule, whose operating characteristics are
exact binomial values (the 2026-10-17 audit):

  pass iff  correct <= KMAX, the largest count whose Wilson 95% upper bound is <= 0.68
            (58 of 100: P(pass) is 0.956 for an indistinguishable engine, 0.378 at a true
            identification rate of 0.60, 0.007 at 0.70)
       and  no stratum reaches 80% correct (16 of 20)
       and  no pair is flagged "visibly wrong"

A session that is identified far BELOW chance is reported (`below_chance`): the reviewer told
the engines apart but labelled them backwards. The verdict follows the rule above, which is
one-sided by design; the warning keeps the inversion from passing unremarked.

Usage: score_tex_2afc.py ANSWERS.json KEY.json [--json]
Exit codes: 0 pass, 1 fail, 2 malformed or incomplete session.
"""
from __future__ import annotations

import argparse
import json
import math
import pathlib
import sys

Z95 = 1.959963984540054
BOUND = 0.68
STRATUM_SHARE = 0.8


def wilson_interval(correct: int, n: int, z: float = Z95) -> tuple[float, float]:
    """The Wilson score interval for `correct` successes in `n` trials."""
    if n <= 0:
        raise ValueError("a session needs at least one pair")
    p = correct / n
    centre = (p + z * z / (2 * n)) / (1 + z * z / n)
    half = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / (1 + z * z / n)
    return centre - half, centre + half


def pass_limit(n: int, bound: float = BOUND) -> int:
    """The largest correct count whose Wilson upper bound stays at or below `bound`."""
    return max(k for k in range(n + 1) if wilson_interval(k, n)[1] <= bound)


def score(answers: dict, key: dict) -> dict:
    if answers.get("kit") != key.get("kit"):
        raise ValueError(f"answers are for kit {answers.get('kit')!r}, the key for {key.get('kit')!r}")
    given = answers.get("answers") or {}
    pairs = key["pairs"]
    missing = [p["pair"] for p in pairs if (given.get(p["pair"]) or {}).get("choice") not in ("left", "right")]
    if missing:
        raise ValueError(f"{len(missing)} pair(s) unanswered, e.g. {missing[:5]}")
    strata: dict[str, list[int]] = {}
    flagged = []
    correct = 0
    for p in pairs:
        answer = given[p["pair"]]
        hit = int(answer["choice"] == p["latex_side"])
        correct += hit
        tally = strata.setdefault(p["stratum"], [0, 0])
        tally[0] += hit
        tally[1] += 1
        if answer.get("visibly_wrong"):
            flagged.append({"pair": p["pair"], "formula": p["formula"], "note": answer.get("note", "")})
    n = len(pairs)
    limit = pass_limit(n)
    lower, upper = wilson_interval(correct, n)
    stratum_rows = {name: {"correct": c, "pairs": m, "limit": math.ceil(STRATUM_SHARE * m),
                           "flagged": c >= math.ceil(STRATUM_SHARE * m)}
                    for name, (c, m) in sorted(strata.items())}
    reasons = []
    if correct > limit:
        reasons.append(f"{correct}/{n} identified exceeds {limit} (Wilson upper {upper:.3f} > {BOUND})")
    reasons += [f"stratum {name}: {row['correct']}/{row['pairs']} reaches {row['limit']}"
                for name, row in stratum_rows.items() if row["flagged"]]
    if flagged:
        reasons.append(f"{len(flagged)} pair(s) flagged visibly wrong")
    return {"schema": "fmn.tex-2afc-score", "version": 1, "kit": key["kit"], "pairs": n, "correct": correct,
            "wilson95": [round(lower, 4), round(upper, 4)], "pass_limit": limit, "strata": stratum_rows,
            "flagged": flagged, "below_chance": correct < n - limit,
            "verdict": "fail" if reasons else "pass", "reasons": reasons}


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("answers", type=pathlib.Path)
    parser.add_argument("key", type=pathlib.Path)
    parser.add_argument("--json", action="store_true", help="print the full report as JSON")
    args = parser.parse_args(argv)
    try:
        report = score(json.loads(args.answers.read_text(encoding="utf-8")),
                       json.loads(args.key.read_text(encoding="utf-8")))
    except (ValueError, KeyError, json.JSONDecodeError) as error:
        print(f"score_tex_2afc: {error}", file=sys.stderr)
        return 2
    if args.json:
        print(json.dumps(report, indent=1, sort_keys=True))
    else:
        print(f"{report['verdict'].upper()}: {report['correct']}/{report['pairs']} identified "
              f"(Wilson 95% {report['wilson95'][0]:.3f}-{report['wilson95'][1]:.3f}; pass at <= {report['pass_limit']})")
        for name, row in report["strata"].items():
            print(f"  {name}: {row['correct']}/{row['pairs']}{'  <- reaches the stratum limit' if row['flagged'] else ''}")
        for item in report["flagged"]:
            print(f"  flagged {item['pair']}: {item['note']}")
        if report["below_chance"]:
            print("  warning: identified below chance; the engines were told apart with the labels swapped")
        for reason in report["reasons"]:
            print(f"  - {reason}")
    return 0 if report["verdict"] == "pass" else 1


if __name__ == "__main__":
    sys.exit(main())
