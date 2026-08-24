"""Small dependency-free validation for the correctness oracle."""

from __future__ import annotations

import json
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
ORACLE = ROOT / "oracle" / "cases.json"

required = {
    "case_id",
    "layer",
    "direction",
    "query",
    "semantic_class",
    "expected_model_path",
    "notes",
}
allowed_directions = {"upstream", "downstream"}
allowed_layers = {"atomic", "integration"}

payload = json.loads(ORACLE.read_text())
assert payload["schema_version"] == 1
cases = payload["cases"]
assert len(cases) == 17
ids = set()

for case in cases:
    assert required <= case.keys(), case
    assert case["case_id"] not in ids, case["case_id"]
    ids.add(case["case_id"])
    assert case["layer"] in allowed_layers, case
    assert case["direction"] in allowed_directions, case
    assert set(case["query"]) == {"model", "column"}, case
    assert isinstance(case["expected_model_path"], list), case
    assert isinstance(case["notes"], str) and case["notes"], case
    if case["direction"] == "upstream":
        assert "expected_terminal_sources" in case, case
        assert case["expected_terminal_sources"] is None or isinstance(
            case["expected_terminal_sources"], list
        ), case
    else:
        assert "expected_downstream_targets" in case, case
        assert isinstance(case["expected_downstream_targets"], list), case

assert ids == {*(f"A{i:02d}" for i in range(1, 13)), *(f"I{i:02d}" for i in range(1, 6))}
print(f"oracle valid: {len(cases)} cases")
