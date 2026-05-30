import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

RUST_ROOTS = [REPO / "crates" / "id4pii"]
JS_ROOTS = [REPO / "extension"]

JS_SKIP_DIRS = {"node_modules", "assets"}
JS_SKIP_SUFFIXES = (".min.js",)
JS_SKIP_NAMES = {"confetti.browser.min.js"}

CHAR_LITERAL = re.compile(r"'(\\u\{[0-9a-fA-F_]+\}|\\x[0-9a-fA-F]{2}|\\.|[^\\'])'")

REGEX_PREV_KEYWORDS = {
    "return", "typeof", "instanceof", "in", "of", "new", "delete", "void",
    "do", "else", "yield", "await", "case", "throw",
}
REGEX_PREV_PUNCT = set("(,=:[!&|?{};+-*%^~<>")


def is_ident_char(ch):
    return ch.isalnum() or ch == "_"


def strip_rust(s):
    out = []
    removed = 0
    n = len(s)
    i = 0
    while i < n:
        c = s[i]
        two = s[i : i + 2]
        prev = s[i - 1] if i > 0 else ""

        if two == "//":
            end = s.find("\n", i)
            end = n if end == -1 else end
            removed += 1
            i = end
            continue

        if two == "/*":
            depth = 1
            j = i + 2
            while j < n and depth > 0:
                if s[j : j + 2] == "/*":
                    depth += 1
                    j += 2
                elif s[j : j + 2] == "*/":
                    depth -= 1
                    j += 2
                else:
                    j += 1
            removed += 1
            i = j
            continue

        if c in "rb" and not is_ident_char(prev):
            rpos = i + 1 if c == "b" and i + 1 < n and s[i + 1] == "r" else (i if c == "r" else None)
            if rpos is not None and rpos + 1 < n and s[rpos + 1] in '"#':
                q = rpos + 1
                hashes = 0
                while q < n and s[q] == "#":
                    hashes += 1
                    q += 1
                if q < n and s[q] == '"':
                    close = '"' + "#" * hashes
                    end = s.find(close, q + 1)
                    end = n if end == -1 else end + len(close)
                    out.append(s[i:end])
                    i = end
                    continue

        if c == '"' or (c == "b" and not is_ident_char(prev) and i + 1 < n and s[i + 1] == '"'):
            start = i + 1 if c == '"' else i + 2
            j = start
            while j < n:
                if s[j] == "\\":
                    j += 2
                    continue
                if s[j] == '"':
                    j += 1
                    break
                j += 1
            out.append(s[i:j])
            i = j
            continue

        if c == "'":
            m = CHAR_LITERAL.match(s, i)
            if m:
                out.append(m.group(0))
                i = m.end()
                continue

        out.append(c)
        i += 1
    return "".join(out), removed


def regex_allowed(prev_char, prev_word):
    if prev_word is not None:
        return prev_word in REGEX_PREV_KEYWORDS
    if prev_char is None:
        return True
    return prev_char in REGEX_PREV_PUNCT or prev_char == "/"


def strip_js(s):
    out = []
    removed = 0
    n = len(s)
    i = 0
    prev_char = None
    prev_word = None
    while i < n:
        c = s[i]
        two = s[i : i + 2]

        if two == "//":
            end = s.find("\n", i)
            end = n if end == -1 else end
            removed += 1
            i = end
            continue

        if two == "/*":
            end = s.find("*/", i + 2)
            end = n if end == -1 else end + 2
            removed += 1
            i = end
            continue

        if c.isspace():
            out.append(c)
            i += 1
            continue

        if c == '"' or c == "'":
            j = i + 1
            while j < n:
                if s[j] == "\\":
                    j += 2
                    continue
                if s[j] == c:
                    j += 1
                    break
                if s[j] == "\n":
                    break
                j += 1
            out.append(s[i:j])
            prev_char = c
            prev_word = None
            i = j
            continue

        if c == "`":
            j = i + 1
            depth = 0
            while j < n:
                if s[j] == "\\":
                    j += 2
                    continue
                if s[j : j + 2] == "${":
                    depth += 1
                    j += 2
                    continue
                if s[j] == "}" and depth > 0:
                    depth -= 1
                    j += 1
                    continue
                if s[j] == "`" and depth == 0:
                    j += 1
                    break
                j += 1
            out.append(s[i:j])
            prev_char = "`"
            prev_word = None
            i = j
            continue

        if c == "/":
            if regex_allowed(prev_char, prev_word):
                j = i + 1
                in_class = False
                ok = False
                while j < n:
                    ch = s[j]
                    if ch == "\\":
                        j += 2
                        continue
                    if ch == "\n":
                        break
                    if ch == "[":
                        in_class = True
                    elif ch == "]":
                        in_class = False
                    elif ch == "/" and not in_class:
                        j += 1
                        ok = True
                        break
                    j += 1
                if ok:
                    while j < n and s[j].isalpha():
                        j += 1
                    out.append(s[i:j])
                    prev_char = ")"
                    prev_word = None
                    i = j
                    continue
            out.append(c)
            prev_char = "/"
            prev_word = None
            i += 1
            continue

        if c.isalpha() or c == "_" or c == "$":
            j = i
            while j < n and (s[j].isalnum() or s[j] in "_$"):
                j += 1
            word = s[i:j]
            out.append(word)
            prev_char = word[-1]
            prev_word = word
            i = j
            continue

        out.append(c)
        prev_char = c
        prev_word = None
        i += 1
    return "".join(out), removed


def tidy(text):
    lines = [line.rstrip() for line in text.split("\n")]
    result = []
    blanks = 0
    for line in lines:
        if line == "":
            blanks += 1
            if blanks > 1:
                continue
        else:
            blanks = 0
        result.append(line)
    while result and result[-1] == "":
        result.pop()
    return "\n".join(result) + "\n"


def collect():
    files = []
    for root in RUST_ROOTS:
        for p in root.rglob("*.rs"):
            if "target" not in p.parts:
                files.append((p, strip_rust))
    for root in JS_ROOTS:
        for p in root.rglob("*.js"):
            if JS_SKIP_DIRS & set(p.parts):
                continue
            if p.name in JS_SKIP_NAMES or p.name.endswith(JS_SKIP_SUFFIXES):
                continue
            files.append((p, strip_js))
    return files


def main():
    total_removed = 0
    changed = 0
    for path, stripper in collect():
        src = path.read_text(encoding="utf-8")
        stripped, removed = stripper(src)
        cleaned = tidy(stripped)
        if cleaned != src:
            path.write_text(cleaned, encoding="utf-8", newline="\n")
            changed += 1
            total_removed += removed
            print(f"  {path.relative_to(REPO)}: -{removed} comment runs")
    print(f"stripped {total_removed} comment runs across {changed} files")


if __name__ == "__main__":
    sys.exit(main())
