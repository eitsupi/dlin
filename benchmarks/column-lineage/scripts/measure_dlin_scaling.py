#!/usr/bin/env python3
"""Measure how dlin column-lineage queries scale across synthetic profiles.

This is a development check, not a published comparison. It times one or
more dlin binaries on the synthetic profiles, with and without a warm cache,
and prints each family with the ratio to the previous, smaller profile next
to the ratio of their scales. When the time ratio stays at or below the scale
ratio, the query scales linearly with the width, depth or number of branches.

It also compares the JSON output of every binary with that of the first one,
so a change meant only to speed dlin up can be checked not to change results.
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCALABILITY = ROOT / "results" / "local" / "scalability"
GENERATOR = ROOT / "scripts" / "generate_scalability_artifacts.py"
DEFAULT_PROFILES = [
    "wide-25",
    "wide-50",
    "wide-100",
    "wide-200",
    "deep-8",
    "deep-16",
    "deep-32",
    "deep-64",
    "fanout-8",
    "fanout-32",
    "fanout-128",
]
DIMENSION = {"volume": "background_models", "wide": "width", "deep": "depth", "fanout": "branches"}


def model_name(unique_id: str) -> str:
    return unique_id.rsplit(".", 1)[-1]


def ensure_profile(name: str) -> dict:
    workload = SCALABILITY / name / "workload.json"
    if not workload.is_file():
        completed = subprocess.run(
            [sys.executable, str(GENERATOR), "--profile", name, "--output-root", str(SCALABILITY)],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        )
        if completed.returncode != 0:
            sys.exit(
                f"could not generate {name}; run ./scripts/regenerate_artifacts.sh first\n{completed.stderr}"
            )
    return json.loads(workload.read_text())


def queries(name: str, workload: dict) -> list[tuple[str, list[str]]]:
    """The queries measured on a profile, as (label, dlin arguments)."""
    manifest = str(SCALABILITY / name / "manifest.json")
    common = ["--manifest-path", manifest, "-o", "json"]
    if workload["family"] == "fanout":
        query = workload["selected_queries"]["downstream"]
        return [
            (
                "downstream",
                ["column", "downstream", model_name(query["model"]), "--column", query["column"], *common],
            )
        ]
    query = workload["selected_queries"]["upstream"]
    model = model_name(query["model"])
    measured = [("upstream", ["column", "upstream", model, "--column", query["column"], *common])]
    if workload["family"] == "wide":
        measured.append(("whole_model", ["column", "upstream", model, *common]))
    return measured


def median_seconds(command: list[str], runs: int, warmup: int) -> float:
    with tempfile.TemporaryDirectory() as scratch:
        export = Path(scratch) / "hyperfine.json"
        subprocess.run(
            [
                "hyperfine",
                "--style",
                "none",
                "--runs",
                str(runs),
                "--warmup",
                str(warmup),
                "--export-json",
                str(export),
                "--command-name",
                "dlin",
                f"{shlex.join(command)} > /dev/null",
            ],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=True,
        )
        return json.loads(export.read_text())["results"][0]["median"]


def run_output(command: list[str]) -> str:
    completed = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, check=False)
    if completed.returncode != 0:
        sys.exit(f"command failed ({completed.returncode}): {shlex.join(command)}\n{completed.stderr}")
    return completed.stdout


def json_value(text: str) -> object:
    return json.loads(text)


def same_json(left: object, right: object) -> bool:
    """Whether two parsed JSON values are equal.

    Key order and number spelling (`1` and `1.0`) do not count; array order
    does, and a boolean never equals a number.
    """
    if isinstance(left, bool) or isinstance(right, bool):
        return type(left) is type(right) and left == right
    if isinstance(left, dict) and isinstance(right, dict):
        return left.keys() == right.keys() and all(same_json(left[key], right[key]) for key in left)
    if isinstance(left, list) and isinstance(right, list):
        return len(left) == len(right) and all(same_json(a, b) for a, b in zip(left, right))
    if isinstance(left, (dict, list)) or isinstance(right, (dict, list)):
        return False
    return left == right


def measure(binaries: list[tuple[str, str]], profiles: list[str], runs: int, warmup: int) -> dict:
    rows = []
    mismatches = []
    for name in profiles:
        workload = ensure_profile(name)
        for label, arguments in queries(name, workload):
            row = {
                "profile": name,
                "family": workload["family"],
                "scale": workload["parameters"][DIMENSION[workload["family"]]],
                "query": label,
                "times": {},
            }
            reference = None
            for binary_label, binary in binaries:
                cache = Path(tempfile.mkdtemp(prefix="dlin-scaling-"))
                try:
                    cold = [binary, *arguments, "--no-cache"]
                    fill = [binary, *arguments, "--cache-dir", str(cache), "--refresh-cache"]
                    warm = [binary, *arguments, "--cache-dir", str(cache)]
                    outputs = {"no_cache": run_output(cold), "fill": run_output(fill), "warm": run_output(warm)}
                    row["times"][binary_label] = {
                        "no_cache": median_seconds(cold, runs, warmup),
                        "warm": median_seconds(warm, runs, warmup),
                    }
                finally:
                    shutil.rmtree(cache, ignore_errors=True)
                values = {mode: json_value(text) for mode, text in outputs.items()}
                if not (
                    same_json(values["no_cache"], values["fill"])
                    and same_json(values["no_cache"], values["warm"])
                ):
                    mismatches.append(f"{name} {label} {binary_label}: outputs differ between cache modes")
                if reference is None:
                    reference = (binary_label, values["no_cache"])
                elif not same_json(values["no_cache"], reference[1]):
                    mismatches.append(f"{name} {label}: {binary_label} output differs from {reference[0]}")
            rows.append(row)
            print(f"measured {name} {label}", file=sys.stderr)
    return {"runs": runs, "warmup": warmup, "binaries": [label for label, _ in binaries], "rows": rows, "mismatches": mismatches}


def markdown(result: dict) -> str:
    labels = result["binaries"]
    header = ["Profile", "Query", "Scale"]
    for label in labels:
        header.extend([f"{label} no cache", f"{label} warm"])
    lines = [
        f"Median of {result['runs']} runs after {result['warmup']} warmup. Each ratio is to the previous profile of the same family and query, shown as time ratio / scale ratio: a time ratio at or below the scale ratio is linear or better.",
        "",
        "| " + " | ".join(header) + " |",
        "| --- | --- | ---: | " + " | ".join(["---:"] * (len(header) - 3)) + " |",
    ]
    previous: dict[tuple[str, str], dict] = {}
    for row in result["rows"]:
        before = previous.get((row["family"], row["query"]))
        cells = [row["profile"], row["query"], str(row["scale"])]
        for label in labels:
            for mode in ("no_cache", "warm"):
                seconds = row["times"][label][mode]
                cell = f"{seconds * 1000:.1f} ms"
                if before is not None:
                    scale = row["scale"] / before["scale"]
                    cell += f" ({seconds / before['times'][label][mode]:.2f}x / {scale:g}x)"
                cells.append(cell)
        lines.append("| " + " | ".join(cells) + " |")
        previous[(row["family"], row["query"])] = row
    lines.append("")
    if result["mismatches"]:
        lines.append("Output mismatches:")
        lines.extend(f"- {item}" for item in result["mismatches"])
    else:
        lines.append("All outputs match across binaries and cache modes.")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument(
        "--dlin",
        action="append",
        metavar="LABEL=PATH",
        help="a dlin binary to measure; repeat to compare (default: dlin on PATH)",
    )
    parser.add_argument("--profile", action="append", help=f"profiles to measure (default: {' '.join(DEFAULT_PROFILES)})")
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--warmup", type=int, default=1)
    parser.add_argument("--json", type=Path, help="also write the measurements to this file")
    args = parser.parse_args()
    binaries = []
    for item in args.dlin or ["dlin=dlin"]:
        label, sep, path = item.partition("=")
        if not sep or not label or not path:
            parser.error(f"--dlin expects LABEL=PATH, got {item!r}")
        if any(label == existing for existing, _ in binaries):
            parser.error(f"--dlin label {label!r} is given more than once")
        resolved = shutil.which(path)
        if resolved is None:
            parser.error(f"dlin binary not found: {path}")
        # The binaries run from the benchmark directory, so a relative path
        # is fixed against the directory the script was started from.
        binaries.append((label, os.path.abspath(resolved)))
    if shutil.which("hyperfine") is None:
        parser.error("hyperfine is not on PATH")
    result = measure(binaries, args.profile or DEFAULT_PROFILES, args.runs, args.warmup)
    if args.json:
        args.json.write_text(json.dumps(result, indent=2) + "\n")
    print(markdown(result))
    return 1 if result["mismatches"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
