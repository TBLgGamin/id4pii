#!/usr/bin/env python3
"""Fetch the Microsoft Presidio-research synthetic PII dataset and convert it to the compact,
fast-to-parse TSV the id4pii benchmark suite consumes.

Source : https://github.com/microsoft/presidio-research (MIT) — data/synth_dataset_v2.json
Output : crates/core/data/pii_dataset.tsv

Each input record has `full_text` and character-offset `spans`. We:
  * convert character offsets to **byte** offsets (id4pii works in bytes; the corpus has
    non-ASCII text so the two differ), validating every span round-trips to its `entity_value`;
  * map Presidio entity types onto id4pii's eight categories, tagging everything outside that
    schema as `other` (the scorer ignores `other`, so the engine is neither rewarded nor
    penalized for entity types it does not target);
  * emit one TSV line per example: `escaped_text \t start:end:category|start:end:category|...`
    where the text field escapes `\\ \t \r \n` so every record is exactly one line.

Run from anywhere: `python scripts/fetch-pii-dataset.py`.
"""

from __future__ import annotations

import json
import sys
import urllib.request
from pathlib import Path

SOURCE_URL = (
    "https://raw.githubusercontent.com/microsoft/presidio-research/master/data/"
    "synth_dataset_v2.json"
)

CATEGORY_MAP = {
    "PERSON": "private_person",
    "STREET_ADDRESS": "private_address",
    "EMAIL_ADDRESS": "private_email",
    "PHONE_NUMBER": "private_phone",
    "DOMAIN_NAME": "private_url",
    "DATE_TIME": "private_date",
    "CREDIT_CARD": "account_number",
    "IBAN_CODE": "account_number",
    "US_SSN": "account_number",
    "US_DRIVER_LICENSE": "account_number",
}

def escape(text: str) -> str:
    return (
        text.replace("\\", "\\\\")
        .replace("\t", "\\t")
        .replace("\r", "\\r")
        .replace("\n", "\\n")
    )

def main() -> int:
    repo_root = Path(__file__).resolve().parent.parent
    out_path = repo_root / "crates" / "core" / "data" / "pii_dataset.tsv"
    out_path.parent.mkdir(parents=True, exist_ok=True)

    print(f"downloading {SOURCE_URL}")
    with urllib.request.urlopen(SOURCE_URL) as resp:
        data = json.loads(resp.read().decode("utf-8"))

    rows: list[str] = []
    total_spans = 0
    mapped_spans = 0
    mismatches = 0
    for ex in data:
        text = ex["full_text"]
        text_bytes = text.encode("utf-8")
        spans = []
        for s in ex.get("spans", []):
            byte_start = len(text[: s["start_position"]].encode("utf-8"))
            byte_end = len(text[: s["end_position"]].encode("utf-8"))

            if text_bytes[byte_start:byte_end].decode("utf-8", "replace") != s["entity_value"]:
                mismatches += 1
                continue
            category = CATEGORY_MAP.get(s["entity_type"], "other")
            if category != "other":
                mapped_spans += 1
            total_spans += 1
            spans.append((byte_start, byte_end, category))
        spans.sort()
        encoded = "|".join(f"{a}:{b}:{c}" for a, b, c in spans)
        rows.append(escape(text) + "\t" + encoded)

    if mismatches:
        print(f"ERROR: {mismatches} spans failed byte-offset validation", file=sys.stderr)
        return 1

    out_path.write_text("\n".join(rows) + "\n", encoding="utf-8", newline="\n")
    print(f"wrote {out_path.relative_to(repo_root)}")
    print(f"  examples: {len(rows)}")
    print(f"  spans: {total_spans} ({mapped_spans} mapped to id4pii categories, "
          f"{total_spans - mapped_spans} 'other')")
    print(f"  bytes: {out_path.stat().st_size}")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
