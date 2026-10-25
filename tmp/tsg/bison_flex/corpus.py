#!/usr/bin/env python3
"""Parse a corpus of Bison/Flex `.g` files and classify what the metagrammar
cannot read yet.

Run through corpus.sh, which builds the parser. One XML file is written at a
time and deleted again: the corpus is 13 MB and the full XML would not fit on
the ramdisk.

The point is not pass/fail. It is the list of *constructs* that reject a file:
one row per distinct first token of an ERROR line, with the files that need it.
"""

from __future__ import annotations

import argparse
import collections
import json
import re
import subprocess
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

# Constructs a parse can hide. `LexerLine` swallows anything that is one line,
# so these have to be looked for in the source, not in the tree.
LINE_GAPS = {
    r"^%\{": "prologue %{ ... %}",
    r"^%\}(?!\{)": "epilogue %}",
    r"^%union\b": "%union { ... }",
    r"^%code\b": "%code { ... }",
    r"^%type\b": "%type declaration",
    r"^%define\b": "%define declaration",
    r"^%expect\b": "%expect declaration",
    r"^%expect-rr\b": "%expect-rr declaration",
    r"^%destructor\b": "%destructor declaration",
    r"^%printer\b": "%printer declaration",
    r"^%locations\b": "%locations",
    r"^%glr-parser\b": "%glr-parser",
    r"^%pure-parser\b": "%pure-parser",
    r"^%parse-param\b": "%parse-param",
    r"^%lex-param\b": "%lex-param",
    r"^%initial-action\b": "%initial-action",
    r"^%require\b": "%require",
    r"^%skeleton\b": "%skeleton",
    r"^%language\b": "%language",
    r"^%output\b": "%output",
    r"^%file-prefix\b": "%file-prefix",
    r"^%debug\b": "%debug",
    r"^%verbose\b": "%verbose",
    r"^%yacc\b": "%yacc",
    r"^%token-table\b": "%token-table",
    r"^%no-lines\b": "%no-lines",
    r"^%dprec\b": "%dprec annotation",
    r"^%merge\b": "%merge annotation",
    r"^%default-prec\b": "%default-prec",
    r"^%no-default-prec\b": "%no-default-prec",
    r"%[{}]": "mid-rule action { ... }",
    r"\b[a-zA-Z_][\w]*\[[^\]]*\]\s*:": "named reference / parameter",
}


def first_error(path: Path, source: Path) -> dict | None:
    try:
        root = ET.parse(path).getroot()
    except ET.ParseError as exc:
        return {"kind": "xml", "detail": str(exc), "row": "?", "line": "", "ancestors": ""}

    def walk(element: ET.Element, ancestors: tuple[str, ...]):
        if element.tag in ("ERROR", "MISSING"):
            return element, ancestors
        for child in element:
            found = walk(child, ancestors + (element.tag,))
            if found is not None:
                return found
        return None

    found = walk(root, ())
    if found is None:
        return None
    element, ancestors = found
    row = int(element.get("srow", "0"))
    line = ""
    lines = source.read_text(errors="replace").splitlines()
    if row < len(lines):
        line = lines[row]
    return {
        "kind": element.tag,
        "row": row + 1,
        "col": int(element.get("scol", "0")) + 1,
        "line": line,
        "ancestors": "/".join(ancestors[-4:]),
    }


def source_gaps(text: str) -> list[str]:
    gaps = []
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        for pattern, label in LINE_GAPS.items():
            if re.search(pattern, stripped) and label not in gaps:
                gaps.append(label)
    return gaps


def signature(error: dict) -> str:
    stripped = error.get("line", "").strip()
    if not stripped:
        return f"{error['kind']} (empty line: {error.get('ancestors', '')})"
    token = stripped.split(None, 1)[0][:40]
    # A directive is the interesting part of a declaration; a rule is not.
    if token.startswith("%"):
        return token[:40]
    return f"{error['kind']}: {token[:40]}"


def parse_one(ts: Path, lib: Path, file: Path, work: Path) -> Path:
    xml = work / "cur.xml"
    with xml.open("wb") as out:
        subprocess.run(
            [str(ts), "parse", "--lib-path", str(lib),
             "--lang-name", "bison_flex", "--xml", str(file)],
            stdout=out, stderr=subprocess.DEVNULL, check=False,
        )
    return xml


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("corpus", type=Path)
    parser.add_argument("--ts", type=Path)
    parser.add_argument("--lib", type=Path)
    parser.add_argument("--work", type=Path, default=Path(".corpus"))
    parser.add_argument("--json", type=Path)
    parser.add_argument("--only", help="substring filter on file names")
    args = parser.parse_args()

    files = sorted(args.corpus.glob("*.g"))
    if args.only:
        files = [f for f in files if args.only in f.name]
    args.work.mkdir(parents=True, exist_ok=True)

    ok, failed = [], []
    for i, file in enumerate(files, 1):
        if args.ts and args.lib:
            xml = parse_one(args.ts, args.lib, file, args.work)
            error = first_error(xml, file)
            xml.unlink()
        else:
            error = {"kind": "not-parsed", "row": "?", "line": "", "ancestors": ""}
        text = file.read_text(errors="replace")
        gaps = source_gaps(text)
        if error is None:
            ok.append((file.name, gaps))
        else:
            failed.append((file.name, error, signature(error), gaps))
        if i % 50 == 0:
            print(f"  ... {i}/{len(files)}", file=sys.stderr)

    by_sig = collections.Counter(item[2] for item in failed)
    examples = collections.defaultdict(list)
    for name, error, sig, _ in failed:
        examples[sig].append((name, error.get("row", "?"), error.get("line", "").strip()[:100]))

    print(f"corpus: {len(files)} files")
    print(f"ok:     {len(ok)}")
    print(f"failed: {len(failed)}")
    print()
    if by_sig:
        print("parse failures by first token of the offending line:")
        for sig, count in by_sig.most_common():
            print(f"{count:5d}  {sig}")
            for name, row, line in examples[sig][:3]:
                print(f"         {name}:{row}  {line}")
        print()

    gap_counts = collections.Counter()
    gap_files = collections.defaultdict(list)
    for name, gaps in ok:
        for gap in gaps:
            gap_counts[gap] += 1
            gap_files[gap].append(name)
    if gap_counts:
        print("line-oriented constructs the grammar currently swallows (parse is ok):")
        for gap, count in gap_counts.most_common():
            print(f"{count:5d}  {gap}")
            for name in gap_files[gap][:3]:
                print(f"         {name}")
        print()

    if args.json:
        payload = {
            "files": len(files),
            "ok": [{"file": n, "gaps": g} for n, g in ok],
            "failed": [
                {"file": n, "error": e, "signature": s, "gaps": g}
                for n, e, s, g in failed
            ],
        }
        args.json.write_text(json.dumps(payload, indent=2))
        print(f"wrote {args.json}")


if __name__ == "__main__":
    main()
