"""Validate run-local dbt artifacts and oracle identifiers."""

from __future__ import annotations

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ARTIFACTS = ROOT / "artifacts"
ORACLE = ROOT / "oracle" / "cases.json"


def main() -> None:
    manifest = json.loads((ARTIFACTS / "manifest.json").read_text())
    catalog = json.loads((ARTIFACTS / "catalog.json").read_text())
    oracle = json.loads(ORACLE.read_text())

    assert manifest["metadata"]["send_anonymous_usage_stats"] is False
    assert manifest["metadata"]["dbt_version"] == "1.12.2"

    model_ids = {
        key for key in manifest["nodes"] if key.startswith("model.")
    }
    catalog_model_ids = {
        key for key in catalog["nodes"] if key.startswith("model.")
    }
    source_ids = set(manifest["sources"])
    assert len(model_ids) == 28
    assert len(catalog_model_ids) == 27

    for case in oracle["cases"]:
        model_id = f"model.column_lineage_correctness.{case['query']['model']}"
        assert model_id in model_ids, (case["case_id"], model_id)
        identifiers = []
        if case["direction"] == "upstream":
            identifiers.extend(case["expected_terminal_sources"] or [])
        else:
            identifiers.extend(case["expected_downstream_targets"])
        identifiers.extend(case["expected_model_path"])
        for identifier in identifiers:
            if identifier.startswith("model."):
                assert identifier.rsplit(".", 1)[0] in model_ids, identifier
            elif identifier.startswith("source."):
                parts = identifier.split(".")
                source_table = ".".join(parts[1:3])
                assert any(source_id.endswith("." + source_table) for source_id in source_ids), identifier
            else:
                raise AssertionError(identifier)

    print(
        "artifacts valid: "
        f"manifest_models={len(model_ids)} "
        f"catalog_models={len(catalog_model_ids)} "
        f"oracle_cases={len(oracle['cases'])}"
    )


if __name__ == "__main__":
    sys.exit(main())
