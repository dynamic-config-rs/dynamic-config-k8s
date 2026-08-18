#!/usr/bin/env python3
"""Splits the operator's `--crds` output into one file per CRD.

Each output file is a single VALID JSON document — which is also valid
YAML, so the same files feed helm's `crds/` directory and the kustomize
base without a yaml library anywhere in the loop.
"""

import json
import pathlib
import sys


def main() -> None:
    source, target = sys.argv[1], pathlib.Path(sys.argv[2])
    target.mkdir(parents=True, exist_ok=True)

    raw = pathlib.Path(source).read_text()
    for part in raw.split("\n---\n"):
        if not part.strip():
            continue
        doc = json.loads(part)
        name = doc["metadata"]["name"].split(".")[0] + ".json"
        (target / name).write_text(json.dumps(doc, indent=2) + "\n")


if __name__ == "__main__":
    main()
