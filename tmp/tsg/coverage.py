#!/usr/bin/env python3
"""Does the language description cover the whole language surface?

`examples/tsg.tsg` is a tree-sitter grammar *of* `.tsg`, and `selfhost.sh` builds
a parser from it. That parser's `node-types.json` is therefore the grammar's own
statement about which constructs exist. The fixtures in `surface/` are meant to
exercise every one of them, so:

    every *named* node type in node-types.json must occur in the parse of
    surface/*.tsg

Anything uncovered is a construct the description knows about but no fixture
spells; the other direction is covered by the fact that `selfhost.sh` requires
those same fixtures to parse without an `ERROR`/`MISSING` node, so a construct a
fixture spells and the description does not understand fails there. Together the
two pin the description to the compiler's actual surface.

Anonymous tokens (`%precedence`, `(`, `|`, …) are deliberately not tracked: the
parse output does not print them, and a fixture that spells one wrongly produces
an `ERROR` node instead, which the other check catches.

Usage: coverage.py <node-types.json> <parse-output>
Exit 0 when covered, 1 (listing what is missing) when not.
"""

import json
import sys


def node_types(value, out):
    """Every *named* `"type"` anywhere in node-types.json."""
    if isinstance(value, dict):
        kind = value.get("type")
        if isinstance(kind, str) and value.get("named"):
            out.add(kind)
        for item in value.values():
            node_types(item, out)
    elif isinstance(value, list):
        for item in value:
            node_types(item, out)


def main(argv):
    if len(argv) != 3:
        sys.stderr.write(__doc__.split("Usage:")[1].strip() + "\n")
        return 2
    with open(argv[1], encoding="utf-8") as handle:
        document = json.load(handle)  # a malformed file is a failure here
    expected = set()
    node_types(document, expected)
    with open(argv[2], encoding="utf-8", errors="replace") as handle:
        tree = handle.read()

    missing = [kind for kind in sorted(expected) if "(%s " % kind not in tree]

    if not missing:
        print("every %d node type in node-types.json occurs in surface/" % len(expected))
        return 0
    print("surface/ never spells %d of %d node types:" % (len(missing), len(expected)))
    for kind in missing:
        print("  %s" % kind)
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
