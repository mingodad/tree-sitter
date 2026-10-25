#!/usr/bin/env python3
"""Structural comparison of two tree-sitter `grammar.json` documents.

Object key order is ignored (JSON objects are unordered), except for the
`rules` object, where order is semantic: the first rule is the start rule.
Array order is always significant. An absent `precedences`/`conflicts`/
`externals`/`inline`/`supertypes`/`reserved` key is equal to an empty one.
"""

import json
import sys

# `parse_grammar` treats an absent collection and an empty one identically, and
# hand-written `grammar.json` files (unlike `dsl.js`) tend to omit the keys. The
# forward compiler always writes them, matching `dsl.js`, so forget the
# difference here rather than in the compiler.
EMPTY_COLLECTIONS = {
    "precedences": [],
    "conflicts": [],
    "externals": [],
    "inline": [],
    "supertypes": [],
    "reserved": {},
}


def fill_empty_collections(doc):
    for key, empty in EMPTY_COLLECTIONS.items():
        if key not in doc:
            doc[key] = type(empty)()


def walk(expected, actual, path, diffs, limit):
    if len(diffs) >= limit:
        return
    if type(expected) is not type(actual):
        diffs.append(f"{path}: type {type(expected).__name__} != {type(actual).__name__}")
        return
    if isinstance(expected, dict):
        missing = [k for k in expected if k not in actual]
        extra = [k for k in actual if k not in expected]
        for k in missing:
            diffs.append(f"{path}.{k}: missing in actual")
        for k in extra:
            diffs.append(f"{path}.{k}: unexpected in actual")
        for k in expected:
            if k in actual:
                walk(expected[k], actual[k], f"{path}.{k}", diffs, limit)
    elif isinstance(expected, list):
        if len(expected) != len(actual):
            diffs.append(f"{path}: length {len(expected)} != {len(actual)}")
        for i, (e, a) in enumerate(zip(expected, actual)):
            walk(e, a, f"{path}[{i}]", diffs, limit)
    elif expected != actual:
        diffs.append(f"{path}: {expected!r} != {actual!r}")


def main():
    name, expected_path, actual_path = sys.argv[1:4]
    with open(expected_path) as f:
        expected = json.load(f)
    with open(actual_path) as f:
        actual = json.load(f)

    # The JS DSL always adds a `$schema` link; it is not part of the contract.
    expected.pop("$schema", None)
    actual.pop("$schema", None)
    fill_empty_collections(expected)
    fill_empty_collections(actual)

    diffs = []
    walk(expected, actual, "", diffs, 12)

    expected_rules = list(expected.get("rules", {}))
    actual_rules = list(actual.get("rules", {}))
    if expected_rules != actual_rules:
        diffs.append(
            f"rules order: {expected_rules} != {actual_rules}"
            "  (the first rule is the start rule)"
        )

    if diffs:
        print(f"FAIL {name}")
        for d in diffs:
            print(f"     {d.lstrip('.')}")
        return 1

    print(f"ok   {name}  ({len(expected_rules)} rules)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
