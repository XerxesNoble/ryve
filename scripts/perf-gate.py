#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright 2026 Loomantix
"""Performance regression gate.

Reads `perf-budgets.toml` and walks `target/criterion/<bench>/new/estimates.json`
for every benchmark listed there. Fails (exit 1) if any benchmark's mean exceeds
its budget * (1 + tolerance_pct / 100). Prints a small status table either way.

Spark ryve-5b9c5d93 — performance regression harness.

Usage:
    python3 scripts/perf-gate.py            # uses ./perf-budgets.toml
    python3 scripts/perf-gate.py --budgets path/to/perf-budgets.toml
    python3 scripts/perf-gate.py --target-dir path/to/target
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

try:
    # Python 3.11+
    import tomllib  # type: ignore[import-not-found]
except ModuleNotFoundError:  # pragma: no cover - older Python fallback
    import tomli as tomllib  # type: ignore[import-not-found]


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument(
        "--budgets",
        default="perf-budgets.toml",
        type=Path,
        help="Path to the perf budgets TOML file.",
    )
    p.add_argument(
        "--target-dir",
        default="target",
        type=Path,
        help="Cargo target dir (criterion writes under <target>/criterion/).",
    )
    return p.parse_args()


def load_budgets(path: Path) -> tuple[dict[str, dict], float]:
    if not path.is_file():
        sys.exit(f"perf-gate: budgets file not found: {path}")
    with path.open("rb") as f:
        doc = tomllib.load(f)
    benches = doc.get("benchmarks", {})
    default_tol = float(doc.get("default_tolerance_pct", 50))
    return benches, default_tol


def read_mean_ns(target_dir: Path, bench_name: str) -> float | None:
    estimates = target_dir / "criterion" / bench_name / "new" / "estimates.json"
    if not estimates.is_file():
        return None
    with estimates.open("r", encoding="utf-8") as f:
        data = json.load(f)
    return float(data["mean"]["point_estimate"])


def fmt_ns(ns: float) -> str:
    if ns >= 1e9:
        return f"{ns / 1e9:.2f}s"
    if ns >= 1e6:
        return f"{ns / 1e6:.2f}ms"
    if ns >= 1e3:
        return f"{ns / 1e3:.2f}us"
    return f"{ns:.0f}ns"


def main() -> int:
    args = parse_args()
    benches, default_tol = load_budgets(args.budgets)

    if not benches:
        print("perf-gate: no benchmarks listed in budget file; nothing to gate.")
        return 0

    rows: list[tuple[str, str, str, str, str]] = []
    failures: list[str] = []
    missing: list[str] = []

    for name, spec in benches.items():
        budget_ns = float(spec["budget_ns"])
        tol_pct = float(spec.get("tolerance_pct", default_tol))
        ceiling = budget_ns * (1.0 + tol_pct / 100.0)

        mean = read_mean_ns(args.target_dir, name)
        if mean is None:
            missing.append(name)
            rows.append((name, "—", fmt_ns(budget_ns), f"+{tol_pct:.0f}%", "MISSING"))
            continue

        status = "OK" if mean <= ceiling else "FAIL"
        rows.append(
            (name, fmt_ns(mean), fmt_ns(budget_ns), f"+{tol_pct:.0f}%", status)
        )
        if status == "FAIL":
            failures.append(
                f"  {name}: mean={fmt_ns(mean)} > ceiling={fmt_ns(ceiling)} "
                f"(budget={fmt_ns(budget_ns)} +{tol_pct:.0f}%)"
            )

    headers = ("benchmark", "mean", "budget", "tol", "status")
    widths = [
        max(len(h), max((len(r[i]) for r in rows), default=0))
        for i, h in enumerate(headers)
    ]
    sep = "  "
    print(sep.join(h.ljust(widths[i]) for i, h in enumerate(headers)))
    print(sep.join("-" * w for w in widths))
    for row in rows:
        print(sep.join(c.ljust(widths[i]) for i, c in enumerate(row)))

    if missing:
        print(
            "\nperf-gate: WARNING — no criterion results found for: "
            + ", ".join(missing),
            file=sys.stderr,
        )
        print(
            "perf-gate: did you run `cargo bench` for the relevant crates first?",
            file=sys.stderr,
        )

    if failures:
        print("\nperf-gate: FAILED:", file=sys.stderr)
        for line in failures:
            print(line, file=sys.stderr)
        return 1

    if missing:
        # Treat missing data as a soft failure: budgets were declared but
        # no measurement is present. Better to flag than to silently pass.
        return 2

    print("\nperf-gate: all benchmarks within budget.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
