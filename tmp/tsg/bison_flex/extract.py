#!/usr/bin/env python3
"""Count and summarise a Bison/Flex `.g` parse tree.

Reads the XML produced by `tree-sitter parse --xml`, so it only needs the
standard library. This is the first consumer of bison_flex/grammar.tsg: a
translator can use the same tree, with much more detail.
"""

from __future__ import annotations

import argparse
import collections
import json
import xml.etree.ElementTree as ET
from pathlib import Path


DIRECTIVES = {
    "PercentToken": "token",
    "PercentFallback": "fallback",
    "PercentLeft": "left",
    "PercentRight": "right",
    "PercentNonassoc": "nonassoc",
    "PercentPrecedence": "precedence",
    "PercentStart": "start",
    "PercentTokenIndent": "token_indent",
    "PercentTokenDedent": "token_dedent",
    "PercentTokenSymbolTable": "token_symbol_table",
}


def text(element: ET.Element) -> str:
    return "".join(element.itertext())


def summary(path: Path) -> dict:
    root = ET.parse(path).getroot()
    tags = collections.Counter(element.tag for element in root.iter())
    source = root.find("source/source_file")
    if source is None:
        raise SystemExit(f"{path}: no source_file node")

    directives = collections.Counter()
    token_names = 0
    starts: list[str] = []
    for directive in source.iter("directive"):
        kind = next(
            (DIRECTIVES[child.tag] for child in directive if child.tag in DIRECTIVES),
            "other",
        )
        directives[kind] += 1
        if kind == "token":
            token_names += sum(1 for _ in directive.iter("decl_token"))
        elif kind == "start":
            name = next((child for child in directive if child.tag == "Name"), None)
            if name is not None:
                starts.append(text(name))

    rules = tags["grule"]
    alternatives = tags["Pipe"] + rules
    precedence_levels = (
        directives["left"]
        + directives["right"]
        + directives["nonassoc"]
        + directives["precedence"]
    )

    return {
        "path": str(path),
        "directives": dict(sorted(directives.items())),
        "precedence_levels": precedence_levels,
        "token_directives": directives["token"],
        "token_names": token_names,
        "start": starts,
        "rules": rules,
        "alternatives": alternatives,
        "empty_alternatives": tags["PercentEmpty"],
        "prec_annotations": tags["PercentPrec"],
        "production_aliases": tags["ProdAlias"],
        "lexer_lines": tags["LexerLine"],
        "section_separators": tags["SectionSep"],
        "names": tags["Name"],
        "literals": tags["Literal"],
        "errors": tags["ERROR"],
        "missing": tags["MISSING"],
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("xml", type=Path, nargs="+")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()

    summaries = [summary(path) for path in args.xml]
    if args.json:
        print(json.dumps(summaries, indent=2))
        return

    for item in summaries:
        print(item["path"])
        print(f"  rules:              {item['rules']}")
        print(f"  alternatives:       {item['alternatives']}")
        print(f"  token directives:   {item['token_directives']}")
        print(f"  token names:        {item['token_names']}")
        print(f"  precedence levels:  {item['precedence_levels']}")
        print(f"  empty alternatives: {item['empty_alternatives']}")
        print(f"  %prec annotations:  {item['prec_annotations']}")
        print(f"  production aliases: {item['production_aliases']}")
        print(f"  lexer lines:        {item['lexer_lines']}")
        print(f"  section separators: {item['section_separators']}")
        if item["errors"] or item["missing"]:
            print(f"  ERROR/MISSING:      {item['errors']}/{item['missing']}")


if __name__ == "__main__":
    main()
