#!/usr/bin/env python3
"""Review quiz questions in the terminal.

    review.py [QUESTIONS_DIR] [--state FILE]

Lists every question file with its answer and explanation. Mark each one ok
or flagged (with a note); marks are saved to a JSON file next to the
directory (questions.review.json by default) after every change, so a
review can be split over several sessions and shared through git.

Keys: j/k or arrows move, PgUp/PgDn/g/G jump, o ok, f flag, F flag with a
note, u unmark, c note, e open in $EDITOR, / filter, t cycle view
(all/unreviewed/flagged/ok/invalid), Esc reset, q quit. Marking moves on to
the next question.
"""

import argparse
import curses
import json
import os
import subprocess
import sys
import textwrap
import tomllib
from pathlib import Path

LETTERS = "ABCDEF"
KEYS = {"question", "choices", "answer", "explanation"}
VIEWS = ["all", "unreviewed", "flagged", "ok", "invalid"]
HELP = "j/k move  o ok  f flag  F flag+why  u unmark  c note  e edit  / filter  t view  ? help  q quit"


class Question:
    def __init__(self, path: Path):
        self.path = path
        self.name = path.stem
        self.load()

    def load(self):
        """Parse the file and check it like the server does."""
        self.data, self.problems = {}, []
        try:
            self.data = tomllib.loads(self.path.read_text())
        except (OSError, tomllib.TOMLDecodeError) as e:
            self.problems.append(str(e))
            return
        d = self.data
        if unknown := set(d) - KEYS:
            self.problems.append(f"unknown keys: {', '.join(sorted(unknown))}")
        for key in sorted(KEYS - set(d)):
            self.problems.append(f"missing {key}")
        choices = d.get("choices", [])
        if not 2 <= len(choices) <= 6:
            self.problems.append(f"needs 2-6 choices, has {len(choices)}")
        if len(set(choices)) != len(choices):
            self.problems.append("duplicate choices")
        if "answer" in d and d["answer"] not in choices:
            self.problems.append("answer is not one of the choices")
        if not str(d.get("explanation", "x")).strip():
            self.problems.append("explanation is empty")

    def haystack(self) -> str:
        d = self.data
        parts = [self.name, d.get("question", ""), d.get("explanation", "")]
        return "\n".join(parts + list(d.get("choices", []))).lower()


class Review:
    def __init__(self, directory: Path, state: Path):
        self.directory = directory
        self.state_path = state
        self.questions = [Question(p) for p in sorted(directory.glob("*.toml"))]
        self.marks: dict[str, dict] = {}
        if state.exists():
            self.marks = json.loads(state.read_text())
        self.view = "all"
        self.filter = ""
        self.cursor = 0
        self.top = 0
        self.message = ""
        self.refresh_visible()

    def save(self):
        tmp = self.state_path.with_suffix(".tmp")
        tmp.write_text(json.dumps(self.marks, indent=2, sort_keys=True, ensure_ascii=False) + "\n")
        tmp.replace(self.state_path)

    def status(self, q: Question) -> str | None:
        return self.marks.get(q.name, {}).get("status")

    def note(self, q: Question) -> str:
        return self.marks.get(q.name, {}).get("note", "")

    def shown(self, q: Question) -> bool:
        s = self.status(q)
        keep = {
            "all": True,
            "unreviewed": s is None,
            "flagged": s == "flag",
            "ok": s == "ok",
            "invalid": bool(q.problems),
        }[self.view]
        return keep and (not self.filter or self.filter.lower() in q.haystack())

    def refresh_visible(self):
        current = self.current()
        self.visible = [q for q in self.questions if self.shown(q)]
        if current in self.visible:
            self.cursor = self.visible.index(current)
        self.cursor = max(0, min(self.cursor, len(self.visible) - 1))

    def current(self) -> Question | None:
        visible = getattr(self, "visible", [])
        return visible[self.cursor] if 0 <= self.cursor < len(visible) else None

    def mark(self, status: str | None):
        q = self.current()
        if not q:
            return
        entry = self.marks.setdefault(q.name, {})
        if status:
            entry["status"] = status
        else:
            entry.pop("status", None)
        if not entry:
            del self.marks[q.name]
        self.save()
        # Keep the list stable under the cursor unless the item left the view.
        if not self.shown(q):
            self.visible.remove(q)
            self.cursor = min(self.cursor, len(self.visible) - 1)
        elif status:
            self.move(1)

    def set_note(self, text: str):
        q = self.current()
        if not q:
            return
        entry = self.marks.setdefault(q.name, {})
        if text:
            entry["note"] = text
        else:
            entry.pop("note", None)
        if not entry:
            del self.marks[q.name]
        self.save()

    def move(self, delta: int):
        if self.visible:
            self.cursor = max(0, min(len(self.visible) - 1, self.cursor + delta))

    def counts(self) -> str:
        ok = sum(self.status(q) == "ok" for q in self.questions)
        flag = sum(self.status(q) == "flag" for q in self.questions)
        bad = sum(bool(q.problems) for q in self.questions)
        total = len(self.questions)
        return f"{ok} ok · {flag} flagged · {total - ok - flag} left · {bad} invalid · {total} total"


def wrap(text: str, width: int) -> list[str]:
    lines = []
    for paragraph in str(text).splitlines() or [""]:
        lines.extend(textwrap.wrap(paragraph, max(10, width)) or [""])
    return lines


class UI:
    def __init__(self, screen, review: Review):
        self.screen = screen
        self.r = review
        curses.curs_set(0)
        curses.use_default_colors()
        curses.init_pair(1, curses.COLOR_BLUE, -1)  # accents
        curses.init_pair(2, curses.COLOR_GREEN, -1)  # ok, correct answer
        curses.init_pair(3, curses.COLOR_RED, -1)  # flagged, invalid
        curses.init_pair(4, curses.COLOR_YELLOW, -1)  # notes
        curses.init_pair(5, curses.COLOR_BLACK, curses.COLOR_CYAN)  # cursor
        self.blue, self.green, self.red, self.yellow, self.sel = (curses.color_pair(i) for i in range(1, 6))

    def put(self, y, x, text, attr=0, width=None):
        h, w = self.screen.getmaxyx()
        if y < 0 or y >= h or x >= w:
            return
        limit = min(w - x, width if width is not None else w)
        if y == h - 1:
            limit -= 1  # writing the bottom-right cell raises
        try:
            self.screen.addstr(y, x, str(text)[: max(0, limit)], attr)
        except curses.error:
            pass

    def draw(self):
        s, r = self.screen, self.r
        s.erase()
        h, w = s.getmaxyx()
        list_w = min(38, max(20, w // 3))
        body_h = h - 2

        # Title and counts.
        view = f"[{r.view}]" + (f" /{r.filter}" if r.filter else "")
        self.put(0, 0, f" nixcon-quiz review {view}", curses.A_BOLD | self.blue)
        counts = r.counts() + " "
        self.put(0, max(0, w - len(counts)), counts, self.blue)

        # List.
        rows = body_h - 1
        if r.cursor < r.top:
            r.top = r.cursor
        elif r.cursor >= r.top + rows:
            r.top = r.cursor - rows + 1
        for i, q in enumerate(r.visible[r.top : r.top + rows]):
            index = r.top + i
            status = r.status(q)
            mark, attr = {"ok": ("✓", self.green), "flag": ("!", self.red)}.get(status, (" ", 0))
            if q.problems:
                mark, attr = "✗", self.red
            line = f" {mark} {q.name}".ljust(list_w - 1)
            if index == r.cursor:
                self.put(1 + i, 0, line, self.sel | curses.A_BOLD, list_w - 1)
            else:
                self.put(1 + i, 0, f" {mark}", attr)
                self.put(1 + i, 3, line[3:], 0, list_w - 4)
        if not r.visible:
            self.put(2, 2, "nothing in this view", curses.A_DIM)
        for y in range(1, body_h):
            self.put(y, list_w - 1, "│", self.blue)

        # Detail.
        q = r.current()
        x, width = list_w + 1, w - list_w - 2
        y = 1
        if q:
            d = q.data
            self.put(y, x, f"{q.path.name}  ({r.cursor + 1}/{len(r.visible)})", self.blue)
            y += 2
            for line in wrap(d.get("question", ""), width):
                self.put(y, x, line, curses.A_BOLD)
                y += 1
            y += 1
            for i, choice in enumerate(d.get("choices", [])):
                correct = choice == d.get("answer")
                prefix = f"{LETTERS[i] if i < 6 else '?'} {'▶' if correct else ' '} "
                attr = self.green | curses.A_BOLD if correct else 0
                for j, line in enumerate(wrap(choice, width - len(prefix))):
                    self.put(y, x, (prefix if j == 0 else " " * len(prefix)) + line, attr)
                    y += 1
            y += 1
            for line in wrap("# " + str(d.get("explanation", "")), width):
                self.put(y, x, line, self.blue)
                y += 1
            y += 1
            for problem in q.problems:
                for line in wrap("✗ " + problem, width):
                    self.put(y, x, line, self.red | curses.A_BOLD)
                    y += 1
            status = r.status(q)
            if status:
                self.put(y, x, "marked ok" if status == "ok" else "flagged", self.green if status == "ok" else self.red)
                y += 1
            if note := r.note(q):
                for line in wrap("note: " + note, width):
                    self.put(y, x, line, self.yellow)
                    y += 1

        # Footer.
        footer = r.message or HELP
        self.put(h - 1, 0, " " + footer, curses.A_DIM if not r.message else self.yellow)
        r.message = ""
        s.refresh()

    def prompt(self, label: str, initial: str = "") -> str | None:
        """One line of input in the footer. Enter accepts, Esc cancels."""
        h, w = self.screen.getmaxyx()
        text = initial
        curses.curs_set(1)
        try:
            while True:
                self.put(h - 1, 0, " " * (w - 1))
                self.put(h - 1, 0, f" {label}{text}", self.yellow)
                self.screen.move(h - 1, min(w - 2, len(label) + len(text) + 1))
                key = self.screen.get_wch()
                if key in ("\n", "\r", curses.KEY_ENTER):
                    return text
                if key == "\x1b":
                    return None
                if key in (curses.KEY_BACKSPACE, "\x7f", "\b"):
                    text = text[:-1]
                elif key == "\x15":  # ^U
                    text = ""
                elif isinstance(key, str) and key.isprintable():
                    text += key
        finally:
            curses.curs_set(0)

    def edit(self):
        q = self.r.current()
        if not q:
            return
        editor = os.environ.get("VISUAL") or os.environ.get("EDITOR") or "vi"
        curses.def_prog_mode()
        curses.endwin()
        subprocess.call([*editor.split(), str(q.path)])
        curses.reset_prog_mode()
        self.screen.clear()
        q.load()
        self.r.message = f"reloaded {q.path.name}" + (" — still invalid" if q.problems else "")

    def run(self):
        r = self.r
        while True:
            self.draw()
            key = self.screen.get_wch()
            h, _ = self.screen.getmaxyx()
            page = max(1, h - 4)
            if key in ("q", "Q"):
                return
            elif key in ("j", curses.KEY_DOWN):
                r.move(1)
            elif key in ("k", curses.KEY_UP):
                r.move(-1)
            elif key in (curses.KEY_NPAGE, " ", "\x06"):
                r.move(page)
            elif key in (curses.KEY_PPAGE, "\x02"):
                r.move(-page)
            elif key in ("g", curses.KEY_HOME):
                r.cursor = 0
            elif key in ("G", curses.KEY_END):
                r.cursor = max(0, len(r.visible) - 1)
            elif key == "o":
                r.mark("ok")
            elif key == "f":
                r.mark("flag")
            elif key == "F":
                if q := r.current():
                    note = self.prompt("why? ", r.note(q))
                    if note is not None:
                        r.set_note(note)
                        r.mark("flag")
            elif key == "u":
                r.mark(None)
            elif key == "c":
                if r.current():
                    note = self.prompt("note: ", r.note(r.current()))
                    if note is not None:
                        r.set_note(note)
            elif key == "e":
                self.edit()
            elif key == "/":
                text = self.prompt("/", r.filter)
                if text is not None:
                    r.filter = text
                    r.cursor = 0
                    r.refresh_visible()
            elif key == "t":
                r.view = VIEWS[(VIEWS.index(r.view) + 1) % len(VIEWS)]
                r.cursor = 0
                r.refresh_visible()
            elif key == "\x1b":
                r.filter, r.view = "", "all"
                r.refresh_visible()
            elif key == "?":
                r.message = "o ok · f flag · F flag with note · u unmark · c note · e $EDITOR · / filter · t view · Esc reset · g/G ends"
            elif key == curses.KEY_RESIZE:
                pass


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("directory", nargs="?", default="questions", type=Path)
    parser.add_argument("--state", type=Path, help="where marks are saved (default: DIRECTORY.review.json)")
    args = parser.parse_args()
    directory = args.directory.resolve()
    if not directory.is_dir():
        sys.exit(f"{args.directory}: not a directory")
    state = args.state or directory.with_name(directory.name + ".review.json")
    review = Review(directory, state)
    if not review.questions:
        sys.exit(f"{args.directory}: no *.toml files")
    os.environ.setdefault("ESCDELAY", "25")
    curses.wrapper(lambda screen: UI(screen, review).run())
    print(review.counts())


if __name__ == "__main__":
    main()
