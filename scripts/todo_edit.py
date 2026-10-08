#!/usr/bin/env python3
"""Verified edits of TODO.md and DECISIONS.md.

Every command reads the file, applies exactly one change, checks the result
and writes it atomically (temporary file + rename). It refuses (exit 1, a
message naming what was missing or ambiguous) instead of guessing.

Structure it relies on:
- a heading is a line starting with `#` at column 0 (outside ``` fences);
- an item starts with `- [ ] ` (or `- [x] `) at column 0 and ends at the
  next item, the next heading or the end of the file, never beyond; its
  trailing blank lines are not part of it;
- a section's own region runs from its heading to the next heading of any
  level, so items under `### Decide` are not items of `## Needs a decision`.

The check after a change: the items of the file, in order and with their
sections, are exactly the expected ones (every other item byte-identical,
the changed one as intended), and every non-item line (headings, intros)
is unchanged. An insertion (`add`, `append-to`, `insert-after`) is also
checked line by line: the file grows by exactly the inserted line count and
every line before and after the insertion is byte-identical, so no line was
joined to its neighbour or split. After writing, the file is read back and
checked again.
"""

from __future__ import annotations

import argparse
import os
import re
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

ITEM_RE = re.compile(r"- \[[ xX]\] ")
HEADING_RE = re.compile(r"(#+)\s*(.*?)\s*$")


class Refusal(Exception):
    """The command cannot be applied safely; nothing was written."""


@dataclass(frozen=True)
class Heading:
    line: int
    level: int
    title: str


@dataclass(frozen=True)
class Item:
    start: int
    end: int  # exclusive, trailing blank lines excluded
    heading: int  # index into Doc.headings, -1 before the first heading
    body: str

    @property
    def text(self) -> str:
        """The item without its marker, whitespace collapsed, for matching."""
        return " ".join(self.body[len("- [ ] ") :].split())

    @property
    def title(self) -> str:
        first = self.body.split("\n", 1)[0]
        return first[len("- [ ] ") :]


class Doc:
    def __init__(self, text: str):
        self.text = text
        self.lines = text.splitlines(keepends=True)
        self.headings: list[Heading] = []
        self.items: list[Item] = []
        # Non-item, non-blank lines with the heading they belong to.
        self.frame: list[tuple[int, str]] = []
        self._parse()

    def _parse(self) -> None:
        in_fence = False
        current_heading = -1
        item_start = None
        item_heading = -1

        def close(at: int) -> None:
            nonlocal item_start
            if item_start is None:
                return
            end = at
            while end > item_start and not self.lines[end - 1].strip():
                end -= 1
            body = "".join(self.lines[item_start:end])
            self.items.append(Item(item_start, end, item_heading, body))
            item_start = None

        for i, line in enumerate(self.lines):
            if line.startswith("```"):
                in_fence = not in_fence
            if not in_fence and line.startswith("#"):
                close(i)
                match = HEADING_RE.match(line)
                level = len(match.group(1)) if match else 1
                title = match.group(2) if match else line.strip()
                self.headings.append(Heading(i, level, title))
                current_heading = len(self.headings) - 1
                self.frame.append((current_heading, line))
                continue
            if not in_fence and ITEM_RE.match(line):
                close(i)
                item_start = i
                item_heading = current_heading
                continue
            if item_start is None and line.strip():
                self.frame.append((current_heading, line))
        close(len(self.lines))

    # Lookups -------------------------------------------------------------

    def heading_path(self, index: int) -> str:
        if index < 0:
            return "(before the first heading)"
        path = [self.headings[index].title]
        level = self.headings[index].level
        for h in reversed(self.headings[:index]):
            if h.level < level:
                path.insert(0, h.title)
                level = h.level
        return " > ".join(path)

    def find_item(self, prefix: str) -> int:
        wanted = " ".join(prefix.split())
        if not wanted:
            raise Refusal("an empty title prefix matches every item")
        matches = [i for i, item in enumerate(self.items) if item.text.startswith(wanted)]
        if not matches:
            raise Refusal(f"no item starts with {prefix!r}")
        if len(matches) > 1:
            listing = "\n".join(
                f"  line {self.items[i].start + 1}: {self.items[i].title.strip()}" for i in matches
            )
            raise Refusal(f"{len(matches)} items start with {prefix!r}; give a longer prefix:\n{listing}")
        return matches[0]

    def find_section(self, name: str) -> int:
        wanted = name.lstrip("#").strip()
        matches = [i for i, h in enumerate(self.headings) if h.title == wanted]
        if not matches:
            known = ", ".join(repr(h.title) for h in self.headings) or "none"
            raise Refusal(f"no heading {wanted!r}; headings: {known}")
        if len(matches) > 1:
            lines = ", ".join(str(self.headings[i].line + 1) for i in matches)
            raise Refusal(f"{len(matches)} headings are titled {wanted!r} (lines {lines})")
        return matches[0]

    def region_end(self, heading: int) -> int:
        """End (exclusive) of a heading's own region: the next heading of any level."""
        if heading + 1 < len(self.headings):
            return self.headings[heading + 1].line
        return len(self.lines)

    def entries(self) -> list[tuple[int, str]]:
        return [(item.heading, item.body.rstrip("\n")) for item in self.items]


# Edits -------------------------------------------------------------------


def read_text_file(path: str) -> str:
    try:
        return Path(path).read_text(encoding="utf-8")
    except OSError as err:
        raise Refusal(f"cannot read --text-file {path}: {err}") from err


def item_lines(text: str) -> list[str]:
    text = text.rstrip()
    if not text or not ITEM_RE.match(text):
        raise Refusal("the item text must start with '- [ ] '")
    lines = (text + "\n").splitlines(keepends=True)
    for line in lines[1:]:
        if line.strip() and not line[0].isspace():
            raise Refusal(f"continuation lines of an item must be indented: {line.rstrip()!r}")
    return lines


def remove_lines(lines: list[str], start: int, end: int) -> list[str]:
    out = lines[:start] + lines[end:]
    if all(not line.strip() for line in out[start:]):
        # The last item of the file: drop the blank gap that led to it.
        del out[start:]
        while out and not out[-1].strip():
            out.pop()
        return out
    # Collapse the blank gap the item leaves behind into the one before it.
    if start == 0 or not out[start - 1].strip():
        while start < len(out) and not out[start].strip() and (start == 0 or not out[start - 1].strip()):
            del out[start]
    return out


@dataclass(frozen=True)
class Insertion:
    """Old lines [first, last) become `added` more lines; the rest stays as it was.

    `lines`, when given, are the exact new lines starting at `first`.
    """

    first: int
    last: int
    added: int
    lines: tuple[str, ...] | None = None


def terminated(lines: list[str], at: int) -> list[str]:
    """A copy of `lines` whose line before `at` ends with a newline."""
    out = list(lines)
    if at > 0 and not out[at - 1].endswith("\n"):
        out[at - 1] += "\n"
    return out


def insert_whole_lines(lines: list[str], at: int, chunk: list[str]) -> tuple[list[str], Insertion]:
    return terminated(lines, at)[:at] + chunk + lines[at:], Insertion(at, at, len(chunk), tuple(chunk))


def insert_block(lines: list[str], at: int, block: list[str], blank_before: bool):
    chunk = list(block)
    if blank_before and at > 0 and lines[at - 1].strip():
        chunk.insert(0, "\n")
    if at < len(lines) and lines[at].strip():
        chunk.append("\n")
    return insert_whole_lines(lines, at, chunk)


def insertion_point(doc: Doc, heading: int, after: str | None, top: bool) -> int:
    start = doc.headings[heading].line
    end = doc.region_end(heading)
    in_section = [i for i, item in enumerate(doc.items) if item.heading == heading]
    if after is not None:
        index = doc.find_item(after)
        if doc.items[index].heading != heading:
            raise Refusal(
                f"item {after!r} is in {doc.heading_path(doc.items[index].heading)!r}, "
                f"not in {doc.heading_path(heading)!r}"
            )
        return doc.items[index].end
    if top and in_section:
        return doc.items[in_section[0]].start
    last = end
    while last > start + 1 and not doc.lines[last - 1].strip():
        last -= 1
    return last


def place_item(doc: Doc, heading: int, block: list[str], after: str | None, top: bool):
    """Insert an item into a section; return the new lines, its ordinal among items and the insertion."""
    at = insertion_point(doc, heading, after, top)
    ordinal = sum(1 for item in doc.items if item.start < at)
    first_item_of_top = top and after is None and any(i.start == at for i in doc.items)
    if first_item_of_top:
        new_lines, insertion = insert_whole_lines(doc.lines, at, block + ["\n"])
    else:
        new_lines, insertion = insert_block(doc.lines, at, block, blank_before=True)
    return new_lines, ordinal, insertion


def cmd_add(doc: Doc, args):
    block = item_lines(read_text_file(args.text_file))
    heading = doc.find_section(args.section)
    new_lines, ordinal, insertion = place_item(doc, heading, block, args.after, args.top)
    expected = doc.entries()
    expected.insert(ordinal, (heading, "".join(block).rstrip("\n")))
    return "".join(new_lines), expected, doc.frame, f"added to {doc.heading_path(heading)!r}", insertion


def cmd_append_to(doc: Doc, args):
    text = read_text_file(args.text_file).rstrip()
    if not text.strip():
        raise Refusal("the text file is empty")
    block = (text + "\n").splitlines(keepends=True)
    for line in block:
        if line.strip() and not line[0].isspace():
            raise Refusal(f"appended lines must be indented: {line.rstrip()!r}")
    index = doc.find_item(args.title)
    item = doc.items[index]
    new_lines, insertion = insert_whole_lines(doc.lines, item.end, block)
    expected = doc.entries()
    expected[index] = (item.heading, item.body.rstrip("\n") + "\n" + text)
    return "".join(new_lines), expected, doc.frame, f"appended to item at line {item.start + 1}", insertion


def cmd_insert_after(doc: Doc, args):
    text = read_text_file(args.text_file)
    index = doc.find_item(args.title)
    item = doc.items[index]
    body = item.body
    count = body.count(args.anchor)
    if count != 1:
        raise Refusal(
            f"the anchor {args.anchor!r} occurs {count} times in the item at line "
            f"{item.start + 1}; it must occur exactly once"
        )
    cut = body.index(args.anchor) + len(args.anchor)
    at_line_end = args.anchor.endswith("\n") or cut == len(body) or body[cut] == "\n"
    if at_line_end:
        # Whole lines go in after the anchor's line, each ending with a newline.
        if not args.anchor.endswith("\n"):
            # The newline the text would start with is the anchor line's own.
            text = text.removeprefix("\n")
            cut = min(cut + 1, len(body))
        text = text.rstrip("\n")
        if not text:
            raise Refusal("the text file is empty")
        text += "\n"
        if not body[:cut].endswith("\n"):
            # The item is the file's unterminated last line.
            body = body + "\n"
            cut = len(body)
        first = item.start + body[:cut].count("\n")
        insertion = Insertion(first, first, text.count("\n"), tuple(text.splitlines(keepends=True)))
    else:
        if not text:
            raise Refusal("the text file is empty")
        first = item.start + body[:cut].count("\n")
        insertion = Insertion(first, first + 1, text.count("\n"))
    new_body = body[:cut] + text + body[cut:]
    before = "".join(doc.lines[: item.start])
    after = "".join(doc.lines[item.end :])
    joiner = "\n" if after and not new_body.endswith("\n") else ""
    expected = doc.entries()
    expected[index] = (item.heading, new_body.rstrip("\n"))
    summary = f"inserted into item at line {item.start + 1}"
    return before + new_body + joiner + after, expected, doc.frame, summary, insertion


def cmd_remove(doc: Doc, args):
    index = doc.find_item(args.title)
    item = doc.items[index]
    new_lines = remove_lines(doc.lines, item.start, item.end)
    expected = doc.entries()
    del expected[index]
    return "".join(new_lines), expected, doc.frame, f"removed item from line {item.start + 1}", None


def cmd_move(doc: Doc, args):
    index = doc.find_item(args.title)
    item = doc.items[index]
    target_name = args.section.lstrip("#").strip()
    doc.find_section(target_name)
    if args.after is not None and doc.find_item(args.after) == index:
        raise Refusal("an item cannot be moved after itself")
    middle = Doc("".join(remove_lines(doc.lines, item.start, item.end)))
    heading = middle.find_section(target_name)
    block = (item.body.rstrip("\n") + "\n").splitlines(keepends=True)
    new_lines, ordinal, _ = place_item(middle, heading, block, args.after, args.top)
    expected = middle.entries()
    expected.insert(ordinal, (heading, item.body.rstrip("\n")))
    return "".join(new_lines), expected, doc.frame, f"moved to {doc.heading_path(heading)!r}", None


def cmd_add_section(doc: Doc, args):
    title = args.title.strip()
    if any(h.title == title for h in doc.headings):
        raise Refusal(f"a heading {title!r} already exists")
    body = read_text_file(args.text_file).strip("\n")
    chunk_text = "#" * args.level + " " + title + "\n" + ("\n" + body + "\n" if body else "")
    chunk = Doc(chunk_text)
    if len(chunk.headings) != 1:
        raise Refusal("the section text must not contain headings of its own")
    base = doc.text.rstrip("\n")
    new_text = (base + "\n\n" if base else "") + chunk_text
    offset = len(doc.headings)
    expected = doc.entries() + [(h + offset, body_) for h, body_ in chunk.entries()]
    frame = doc.frame + [(h + offset, line) for h, line in chunk.frame]
    return new_text, expected, frame, f"added section {title!r}", None


# Verification and writing ------------------------------------------------


def verify_insertion(old_lines: list[str], new_text: str, insertion: Insertion) -> None:
    """Refuse unless only the lines of `insertion` changed and none were joined or split."""
    new_lines = new_text.splitlines(keepends=True)
    first, last, added = insertion.first, insertion.last, insertion.added
    if len(new_lines) != len(old_lines) + added:
        raise Refusal(
            f"the change would join or split lines: the file would have {len(new_lines)} lines, "
            f"expected {len(old_lines)} + {added} inserted"
        )
    # The line before may only have gained the newline a file's last line lacked.
    if new_lines[:first] != terminated(old_lines, first)[:first]:
        raise Refusal(f"the change would alter the lines before the insertion at line {first + 1}")
    tail = len(old_lines) - last
    if new_lines[len(new_lines) - tail :] != old_lines[last:]:
        raise Refusal(f"the change would alter the lines after the insertion at line {first + 1}")
    if insertion.lines is not None and tuple(new_lines[first : first + added]) != insertion.lines:
        raise Refusal(f"the inserted lines at line {first + 1} are not the intended ones")


def verify(new_text: str, expected_items: list, expected_frame: list, old: Doc, insertion=None) -> None:
    if insertion is not None:
        verify_insertion(old.lines, new_text, insertion)
    new = Doc(new_text)
    missing = [h.title for h in old.headings if h.title not in {n.title for n in new.headings}]
    if missing:
        raise Refusal(f"the change would delete headings: {missing}")
    if new.entries() != expected_items:
        raise Refusal("the change would alter items other than the intended one")
    if new.frame != expected_frame:
        raise Refusal("the change would alter text outside the items (headings or intros)")


def atomic_write(path: Path, text: str, original: str) -> None:
    if path.read_text(encoding="utf-8") != original:
        raise Refusal(f"{path} changed while editing; nothing written, run the command again")
    fd, tmp = tempfile.mkstemp(prefix=f".{path.name}.", suffix=".tmp", dir=str(path.parent))
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="") as handle:
            handle.write(text)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(tmp, path.stat().st_mode & 0o7777)
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except FileNotFoundError:
            pass
        raise


COMMANDS = {
    "add": cmd_add,
    "append-to": cmd_append_to,
    "insert-after": cmd_insert_after,
    "remove": cmd_remove,
    "move": cmd_move,
    "add-section": cmd_add_section,
}


def parse_args(argv):
    parser = argparse.ArgumentParser(description="Verified edits of TODO.md and DECISIONS.md.")
    parser.add_argument("--file", default="TODO.md", help="file to edit (default: TODO.md)")
    sub = parser.add_subparsers(dest="command", required=True)

    p = sub.add_parser("find", help="print an item's section and line range")
    p.add_argument("title", help="prefix of the item's text after '- [ ] '")

    p = sub.add_parser(
        "add", help="insert an item at the end of a section or after an item (as whole lines)"
    )
    p.add_argument("section", help="heading title, e.g. 'Next, in order'")
    where = p.add_mutually_exclusive_group()
    where.add_argument("--after", metavar="TITLE_PREFIX")
    where.add_argument("--top", action="store_true", help="before the section's first item")
    p.add_argument("--text-file", required=True)

    p = sub.add_parser(
        "append-to", help="append indented lines to the end of an item (as whole lines, ending with one newline)"
    )
    p.add_argument("title")
    p.add_argument("--text-file", required=True)

    insert_help = (
        "insert text after an anchor that occurs once in the item: when the anchor "
        "ends with a newline or at the end of a line, the text goes in as whole lines "
        "after that line (ending with exactly one newline; with an anchor that stops "
        "before its line's newline, one leading newline of the text is dropped); when "
        "the anchor ends mid-line, the text is inserted literally, byte for byte"
    )
    p = sub.add_parser("insert-after", help=insert_help, description=insert_help)
    p.add_argument("title")
    p.add_argument("--anchor", required=True)
    p.add_argument("--text-file", required=True)

    p = sub.add_parser("remove", help="remove one item")
    p.add_argument("title")

    p = sub.add_parser("move", help="move an item to the end of a section (or --top/--after)")
    p.add_argument("title")
    p.add_argument("section")
    where = p.add_mutually_exclusive_group()
    where.add_argument("--after", metavar="TITLE_PREFIX")
    where.add_argument("--top", action="store_true")

    p = sub.add_parser("add-section", help="append a '## <title>' section with a body at the end")
    p.add_argument("title")
    p.add_argument("--text-file", required=True)
    p.add_argument("--level", type=int, default=2, choices=range(1, 7))
    return parser.parse_args(argv)


def run(argv) -> int:
    args = parse_args(argv)
    path = Path(args.file)
    try:
        original = path.read_text(encoding="utf-8")
    except OSError as err:
        print(f"todo_edit: cannot read {path}: {err}", file=sys.stderr)
        return 1
    doc = Doc(original)
    try:
        if args.command == "find":
            item = doc.items[doc.find_item(args.title)]
            print(f"{doc.heading_path(item.heading)}: lines {item.start + 1}-{item.end}")
            return 0
        new_text, expected, frame, summary, insertion = COMMANDS[args.command](doc, args)
        if new_text == original:
            raise Refusal("the change would leave the file unchanged")
        verify(new_text, expected, frame, doc, insertion)
        atomic_write(path, new_text, original)
        written = path.read_text(encoding="utf-8")
        if written != new_text:
            raise Refusal(f"{path} does not hold the written text after the rename")
        verify(written, expected, frame, doc, insertion)
    except Refusal as err:
        print(f"todo_edit: {err}", file=sys.stderr)
        return 1
    print(f"{path}: {summary}")
    return 0


if __name__ == "__main__":
    sys.exit(run(sys.argv[1:]))
