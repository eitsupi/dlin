"""Run thin representative preflight queries against installed lineage CLIs."""

from __future__ import annotations

import json
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Callable


ROOT = Path(__file__).resolve().parents[1]
RESULTS = ROOT / "results" / "local" / "preflight"
MANIFEST = "artifacts/manifest.json"
CATALOG = "artifacts/catalog.json"
META_ARTIFACT = "results/local/preflight/dbt-meta-lineage.json"
META_ARTIFACT_PATH = ROOT / META_ARTIFACT

Predicate = Callable[[str], str]


def result_path(tool: str, command: str, suffix: str) -> Path:
    return RESULTS / f"{tool}_{command}.{suffix}"


def write_raw(path: Path, content: str) -> str:
    path.write_text(content)
    return path.relative_to(ROOT).as_posix()


def invalid_status(
    tool: str,
    command: str,
    exit_code: int,
    reason: str,
    stdout: str = "",
    stderr: str = "",
) -> dict[str, object]:
    return {
        "tool": tool,
        "command": command,
        "exit_code": exit_code,
        "valid": False,
        "reason": reason,
        "stdout_path": write_raw(result_path(tool, command, "stdout"), stdout),
        "stderr_path": write_raw(result_path(tool, command, "stderr"), stderr),
    }


def run_command(
    tool: str,
    command: str,
    argv: list[str],
    predicate: Predicate | None = None,
) -> dict[str, object]:
    stdout_file = result_path(tool, command, "stdout")
    stderr_file = result_path(tool, command, "stderr")
    try:
        completed = subprocess.run(
            argv,
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
            timeout=30,
        )
    except subprocess.TimeoutExpired as error:
        stdout = error.stdout or ""
        stderr = error.stderr or ""
        if isinstance(stdout, bytes):
            stdout = stdout.decode(errors="replace")
        if isinstance(stderr, bytes):
            stderr = stderr.decode(errors="replace")
        return invalid_status(tool, command, 124, "command timed out after 30 seconds", stdout, stderr)
    except OSError as error:
        return invalid_status(tool, command, 127, f"could not execute command: {error}")

    stdout_path = write_raw(stdout_file, completed.stdout)
    stderr_path = write_raw(stderr_file, completed.stderr)
    if completed.returncode != 0:
        valid = False
        reason = f"command exited with {completed.returncode}"
    elif predicate is None:
        valid = True
        reason = "command succeeded"
    else:
        try:
            reason = predicate(completed.stdout)
            valid = True
        except (
            AssertionError,
            IndexError,
            KeyError,
            StopIteration,
            TypeError,
            ValueError,
            AttributeError,
            json.JSONDecodeError,
        ) as error:
            valid = False
            reason = str(error) or error.__class__.__name__
    return {
        "tool": tool,
        "command": command,
        "exit_code": completed.returncode,
        "valid": valid,
        "reason": reason,
        "stdout_path": stdout_path,
        "stderr_path": stderr_path,
    }


def skipped_status(tool: str, command: str, reason: str) -> dict[str, object]:
    return invalid_status(tool, command, 126, reason)


def reported_version(stdout: str) -> str | None:
    matches = re.findall(
        r"(?<![0-9A-Za-z])v?(\d+\.\d+\.\d+)(?![0-9A-Za-z.+-])", stdout
    )
    return matches[0] if len(matches) == 1 else None


def executable(
    binary: str, expected_version: str, label: str | None = None
) -> tuple[str | None, dict[str, object]]:
    tool = label or binary
    path = shutil.which(binary)
    if path is None:
        return None, invalid_status(tool, "version", 127, "executable is not on PATH")
    resolved = Path(path).resolve()
    path_text = resolved.as_posix()
    if "/target/debug/" in path_text or "/target/release/" in path_text:
        return None, invalid_status(tool, "version", 126, f"development build path rejected: {path_text}")

    def check_version(stdout: str) -> str:
        actual_version = reported_version(stdout)
        assert actual_version == expected_version, (
            f"expected version {expected_version!r}, got {stdout.strip()!r}"
        )
        return f"version {expected_version}"

    status = run_command(tool, "version", [path, "--version"], check_version)
    return (path if status["valid"] else None), status


def json_payload(stdout: str) -> object:
    return json.loads(stdout)


def dlin_upstream(stdout: str) -> str:
    payload = json_payload(stdout)
    assert isinstance(payload, list) and payload, "JSON result is empty"
    entry = next(item for item in payload if item.get("model") == "i01_deep_08")
    assert not entry.get("errors"), "query returned lineage errors"
    column = next(column for column in entry["columns"] if column["column"] == "amount_double")
    sources = column["sources"]
    assert any(
        source.get("column") == "amount"
        and str(source.get("table", "")).split(".")[-1] == "raw_orders"
        for source in sources
    ), "raw_orders.amount was not found"
    return "I01 query and raw_orders.amount source found"


def dlin_downstream(stdout: str) -> str:
    payload = json_payload(stdout)
    assert isinstance(payload, list) and payload, "JSON result is empty"
    entry = next(
        item
        for item in payload
        if item.get("model") == "i05_fanout_base" and item.get("column") == "amount"
    )
    expected = {
        ("i05_direct_consumer", "amount"),
        ("i05_expression_consumer", "scaled_amount"),
        ("i05_rename_consumer", "renamed_amount"),
    }
    actual = {
        (item["model"], item["column"]) for item in entry["impacted_columns"]
    }
    assert expected <= actual, f"missing downstream consumers: {sorted(expected - actual)}"
    return "I05 query and three downstream consumers found"


def parrant_coverage(entry: dict) -> None:
    coverage = entry["coverage"]
    assert coverage["parsed_ok"] > 0, "no models parsed"
    assert coverage["parse_failed"] < coverage["models_in_manifest"], "all models failed to parse"


def parrant_upstream(stdout: str) -> str:
    entry = json_payload(stdout)
    assert entry["model"] == "i01_deep_08" and entry["column"] == "amount_double"
    parrant_coverage(entry)
    raw_orders = entry["upstream"]["models"]["raw_orders"]["amount"]
    assert "raw_orders.amount" in raw_orders["source_columns"], "raw_orders.amount was not found"
    return "I01 query and raw_orders.amount source found"


def parrant_downstream(stdout: str) -> str:
    entry = json_payload(stdout)
    assert entry["model"] == "i05_fanout_base" and entry["column"] == "amount"
    parrant_coverage(entry)
    expected = {
        "i05_direct_consumer": "amount",
        "i05_expression_consumer": "scaled_amount",
        "i05_rename_consumer": "renamed_amount",
    }
    models = entry["downstream"]["models"]
    assert all(model in models and column in models[model] for model, column in expected.items()), (
        "missing downstream consumers"
    )
    return "I05 query and three downstream consumers found"


def meta_build(stdout: str) -> str:
    summary = json_payload(stdout)
    artifact = Path(summary["artifact"])
    if not artifact.is_absolute():
        artifact = ROOT / artifact
    assert artifact == ROOT / META_ARTIFACT, "build wrote an unexpected artifact path"
    assert artifact.is_file(), "lineage artifact was not created"
    payload = json.loads(artifact.read_text())
    assert payload["graph"]["nodes"] and payload["graph"]["edges"], "lineage graph is empty"
    return f"lineage artifact has {len(payload['graph']['nodes'])} nodes and {len(payload['graph']['edges'])} edges"


def meta_upstream(stdout: str) -> str:
    entry = json_payload(stdout)
    assert entry["target"]["id"] == "i01_deep_08.amount_double"
    assert any(item["id"].endswith("raw_orders.amount") for item in entry["all"]), (
        "raw_orders.amount was not found"
    )
    return "I01 query and raw_orders.amount source found"


def meta_downstream(stdout: str) -> str:
    entry = json_payload(stdout)
    assert entry["target"]["id"] == "i05_fanout_base.amount"
    expected = {
        "i05_direct_consumer.amount",
        "i05_expression_consumer.scaled_amount",
        "i05_rename_consumer.renamed_amount",
    }
    actual = {item["id"] for item in entry["all"]}
    assert expected <= actual, f"missing downstream consumers: {sorted(expected - actual)}"
    return "I05 query and three downstream consumers found"


def main() -> int:
    RESULTS.mkdir(parents=True, exist_ok=True)
    META_ARTIFACT_PATH.unlink(missing_ok=True)
    statuses: list[dict[str, object]] = []

    dlin, status = executable("dlin", "0.2.4")
    statuses.append(status)
    if dlin:
        statuses.append(
            run_command(
                "dlin",
                "upstream",
                [
                    dlin,
                    "column",
                    "upstream",
                    "i01_deep_08",
                    "--column",
                    "amount_double",
                    "--manifest-path",
                    MANIFEST,
                    "--no-cache",
                    "-o",
                    "json",
                ],
                dlin_upstream,
            )
        )
        statuses.append(
            run_command(
                "dlin",
                "downstream",
                [
                    dlin,
                    "column",
                    "downstream",
                    "i05_fanout_base",
                    "--column",
                    "amount",
                    "--manifest-path",
                    MANIFEST,
                    "--no-cache",
                    "-o",
                    "json",
                ],
                dlin_downstream,
            )
        )
    else:
        statuses.extend(
            [
                skipped_status("dlin", "upstream", "version check failed"),
                skipped_status("dlin", "downstream", "version check failed"),
            ]
        )

    parrant, status = executable("parrant", "0.17.2")
    statuses.append(status)
    if parrant:
        statuses.append(
            run_command(
                "parrant",
                "upstream",
                [
                    parrant,
                    "--manifest",
                    MANIFEST,
                    "--catalog",
                    CATALOG,
                    "--select",
                    "+i01_deep_08.amount_double",
                    "--format",
                    "json",
                ],
                parrant_upstream,
            )
        )
        statuses.append(
            run_command(
                "parrant",
                "downstream",
                [
                    parrant,
                    "--manifest",
                    MANIFEST,
                    "--catalog",
                    CATALOG,
                    "--select",
                    "i05_fanout_base.amount+",
                    "--format",
                    "json",
                ],
                parrant_downstream,
            )
        )
    else:
        statuses.extend(
            [
                skipped_status("parrant", "upstream", "version check failed"),
                skipped_status("parrant", "downstream", "version check failed"),
            ]
        )

    meta, status = executable("meta", "0.3.8", "dbt-meta")
    statuses.append(status)
    if meta:
        statuses.append(
            run_command(
                "dbt-meta",
                "build",
                [
                    meta,
                    "lineage",
                    "build",
                    "--manifest",
                    MANIFEST,
                    "--catalog",
                    CATALOG,
                    "--output",
                    META_ARTIFACT,
                    "--json",
                    "--no-compile",
                ],
                meta_build,
            )
        )
        build_status = statuses[-1]
        if build_status["valid"]:
            statuses.append(
                run_command(
                    "dbt-meta",
                    "upstream",
                    [meta, "lineage", "column", "--artifact", META_ARTIFACT, "--json", "i01_deep_08.amount_double"],
                    meta_upstream,
                )
            )
            statuses.append(
                run_command(
                    "dbt-meta",
                    "downstream",
                    [meta, "lineage", "downstream", "--artifact", META_ARTIFACT, "--json", "i05_fanout_base.amount"],
                    meta_downstream,
                )
            )
        else:
            statuses.extend(
                [
                    skipped_status("dbt-meta", "upstream", "build invalid; query skipped"),
                    skipped_status("dbt-meta", "downstream", "build invalid; query skipped"),
                ]
            )
    else:
        statuses.extend(
            [
                skipped_status("dbt-meta", "build", "version check failed"),
                skipped_status("dbt-meta", "upstream", "version check failed"),
                skipped_status("dbt-meta", "downstream", "version check failed"),
            ]
        )

    status_path = RESULTS / "status.json"
    status_path.write_text(json.dumps({"schema_version": 1, "commands": statuses}, indent=2) + "\n")
    print(status_path.relative_to(ROOT))
    return 0 if all(command["valid"] for command in statuses) else 1


if __name__ == "__main__":
    sys.exit(main())
