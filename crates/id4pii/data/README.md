# PII evaluation dataset

`pii_dataset.tsv` is the labelled corpus the id4pii benchmark suite runs against — for both
**speed** (throughput over realistic text) and **correctness** (precision / recall / F1 of the
detector against ground-truth spans). It is committed on purpose so benchmarks are reproducible
without a network fetch.

## Source & license

Derived from the **Microsoft Presidio-research** synthetic dataset
(`data/synth_dataset_v2.json`), <https://github.com/microsoft/presidio-research>, which is
distributed under the **MIT License**. The data is synthetic (template-generated fake PII), not
real personal data. Regenerate with:

```sh
python scripts/fetch-pii-dataset.py
```

That script downloads the source, converts character offsets to byte offsets (validating every
span round-trips to its labelled value), maps Presidio entity types onto id4pii's categories,
and writes this file.

## Format

One example per line, UTF-8, LF line endings, no header:

```
<escaped_text> \t <span>|<span>|...
```

- **`escaped_text`** — the source text with `\`, tab, CR and LF escaped as `\\`, `\t`, `\r`,
  `\n`, so each record is exactly one physical line. The loader unescapes it back to the
  original bytes; span offsets index that original (unescaped) text.
- **spans** — `start:end:category` triples joined by `|` (empty when the example has no PII).
  `start`/`end` are **byte** offsets; `category` is an id4pii snake_case category
  (`private_person`, `private_email`, …) or `other`.

## Category mapping

Presidio entity types are mapped onto id4pii's eight categories:

| Presidio | id4pii |
|---|---|
| `PERSON` | `private_person` |
| `STREET_ADDRESS` | `private_address` |
| `EMAIL_ADDRESS` | `private_email` |
| `PHONE_NUMBER` | `private_phone` |
| `DOMAIN_NAME` | `private_url` |
| `DATE_TIME` | `private_date` |
| `CREDIT_CARD`, `IBAN_CODE`, `US_SSN`, `US_DRIVER_LICENSE` | `account_number` |
| `GPE`, `ORGANIZATION`, `TITLE`, `AGE`, `NRP`, `ZIP_CODE`, `IP_ADDRESS` | `other` |

`other` spans mark PII the engine does not target. The scorer treats a prediction overlapping an
`other` span as *don't-care* (neither true nor false positive), so the engine is not penalized
for entity types outside its schema. The corpus contains no `secret`-category examples; that
category is exercised by the unit tests in `detect/regex.rs` instead.

Counts: 1500 examples, 2863 labelled spans (1930 mapped to id4pii categories, 933 `other`).
