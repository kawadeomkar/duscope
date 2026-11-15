"""A TOML reader for the eval's two files, for Pythons without `tomllib`.

`tomllib` arrived in Python 3.11, and the bench harness runs on 3.9 (the
system Python on macOS). `load` uses `tomllib` when it is there and this
reader otherwise; `bench/test_eval.py` checks that both read `tasks.toml` and
`config.toml` alike.

It reads the TOML 1.0 the eval files use: comments, `[table]` and `[[array of
tables]]` headers, bare, quoted and dotted keys, basic and literal strings
(single- and multi-line), integers, floats, booleans, arrays and inline
tables. Dates and times are refused, as are a key or table given twice.
"""

from __future__ import annotations

import re
from typing import Any, Dict, List

try:  # Python 3.11 and later
    import tomllib as _tomllib
except ImportError:  # pragma: no cover - which branch runs depends on the Python
    _tomllib = None


class TomlError(ValueError):
    """The text is not TOML this reader understands."""


def load(path: str) -> Dict[str, Any]:
    """The TOML file at `path`, read with `tomllib` when the Python has it."""
    with open(path, "rb") as f:
        data = f.read()
    if _tomllib is not None:
        return _tomllib.loads(data.decode("utf-8"))
    return loads(data.decode("utf-8"))


def loads(text: str) -> Dict[str, Any]:
    """The TOML document `text`, read by this module whatever the Python."""
    return _Parser(text).document()


_BARE_KEY = re.compile(r"[A-Za-z0-9_-]+")
_NUMBER = re.compile(
    r"[+-]?(?:0x[0-9A-Fa-f_]+|0o[0-7_]+|0b[01_]+|inf|nan"
    r"|[0-9][0-9_]*(?:\.[0-9_]+)?(?:[eE][+-]?[0-9_]+)?)"
)
_ESCAPES = {"b": "\b", "t": "\t", "n": "\n", "f": "\f", "r": "\r", '"': '"', "\\": "\\"}


class _Parser:
    def __init__(self, text: str) -> None:
        self.s = text.replace("\r\n", "\n")
        self.i = 0
        # Tables made by a header or a dotted key, so a second header fails.
        self.defined: set = set()

    # -- the document ---------------------------------------------------

    def document(self) -> Dict[str, Any]:
        root: Dict[str, Any] = {}
        current = root
        while True:
            self.blank_lines()
            if self.i >= len(self.s):
                return root
            if self.s.startswith("[[", self.i):
                self.i += 2
                keys = self.key_path()
                self.expect("]]")
                current = self.array_table(root, keys)
            elif self.s[self.i] == "[":
                self.i += 1
                keys = self.key_path()
                self.expect("]")
                current = self.table(root, keys)
            else:
                keys = self.key_path()
                self.spaces()
                self.expect("=")
                self.spaces()
                self.assign(current, keys, self.value())
            self.end_of_line()

    def table(self, root: Dict[str, Any], keys: List[str]) -> Dict[str, Any]:
        path = tuple(keys)
        if path in self.defined:
            raise self.error("table [%s] defined twice" % ".".join(keys))
        self.defined.add(path)
        node = root
        for key in keys:
            node = node.setdefault(key, {})
            if isinstance(node, list):
                node = node[-1]
            if not isinstance(node, dict):
                raise self.error("%s is not a table" % key)
        return node

    def array_table(self, root: Dict[str, Any], keys: List[str]) -> Dict[str, Any]:
        node = root
        for key in keys[:-1]:
            node = node.setdefault(key, {})
            if isinstance(node, list):
                node = node[-1]
            if not isinstance(node, dict):
                raise self.error("%s is not a table" % key)
        items = node.setdefault(keys[-1], [])
        if not isinstance(items, list):
            raise self.error("%s is not an array of tables" % keys[-1])
        fresh: Dict[str, Any] = {}
        items.append(fresh)
        return fresh

    def assign(self, table: Dict[str, Any], keys: List[str], value: Any) -> None:
        for key in keys[:-1]:
            table = table.setdefault(key, {})
            if not isinstance(table, dict):
                raise self.error("%s is not a table" % key)
        if keys[-1] in table:
            raise self.error("key %s given twice" % keys[-1])
        table[keys[-1]] = value

    # -- keys -----------------------------------------------------------

    def key_path(self) -> List[str]:
        keys = []
        while True:
            self.spaces()
            keys.append(self.key())
            self.spaces()
            if self.peek(".") and not self.peek(".."):
                self.i += 1
                continue
            return keys

    def key(self) -> str:
        if self.peek('"'):
            return self.basic_string()
        if self.peek("'"):
            return self.literal_string()
        m = _BARE_KEY.match(self.s, self.i)
        if not m:
            raise self.error("a key was expected")
        self.i = m.end()
        return m.group(0)

    # -- values ---------------------------------------------------------

    def value(self) -> Any:
        if self.peek('"""'):
            return self.multiline_basic()
        if self.peek("'''"):
            return self.multiline_literal()
        if self.peek('"'):
            return self.basic_string()
        if self.peek("'"):
            return self.literal_string()
        if self.peek("["):
            return self.array()
        if self.peek("{"):
            return self.inline_table()
        for word, value in (("true", True), ("false", False)):
            if self.s.startswith(word, self.i):
                self.i += len(word)
                return value
        m = _NUMBER.match(self.s, self.i)
        if not m:
            raise self.error("a value was expected")
        text = m.group(0)
        rest = self.s[m.end() : m.end() + 1]
        if rest in ("-", ":") or (rest == "T" and text.isdigit()):
            raise self.error("dates and times are not read")
        self.i = m.end()
        return number(text)

    def array(self) -> List[Any]:
        self.i += 1
        items = []
        while True:
            self.blank_lines()
            if self.peek("]"):
                self.i += 1
                return items
            items.append(self.value())
            self.blank_lines()
            if self.peek(","):
                self.i += 1
            elif not self.peek("]"):
                raise self.error("',' or ']' was expected in an array")

    def inline_table(self) -> Dict[str, Any]:
        self.i += 1
        table: Dict[str, Any] = {}
        self.spaces()
        if self.peek("}"):
            self.i += 1
            return table
        while True:
            keys = self.key_path()
            self.spaces()
            self.expect("=")
            self.spaces()
            self.assign(table, keys, self.value())
            self.spaces()
            if self.peek(","):
                self.i += 1
                continue
            self.expect("}")
            return table

    def basic_string(self) -> str:
        self.i += 1
        out = []
        while True:
            if self.i >= len(self.s) or self.s[self.i] == "\n":
                raise self.error("unterminated string")
            c = self.s[self.i]
            if c == '"':
                self.i += 1
                return "".join(out)
            if c == "\\":
                out.append(self.escape())
                continue
            out.append(c)
            self.i += 1

    def multiline_basic(self) -> str:
        self.i += 3
        if self.peek("\n"):
            self.i += 1
        out = []
        while True:
            if self.i >= len(self.s):
                raise self.error("unterminated string")
            if self.s.startswith('"""', self.i):
                # Up to two quotes may end the content just before the close.
                end = self.i + 3
                while end < len(self.s) and self.s[end] == '"' and end - self.i < 5:
                    end += 1
                out.append('"' * (end - self.i - 3))
                self.i = end
                return "".join(out)
            c = self.s[self.i]
            if c == "\\":
                after = re.match(r"\\[ \t]*\n[ \t\n]*", self.s[self.i :])
                if after:
                    self.i += after.end()
                    continue
                out.append(self.escape())
                continue
            out.append(c)
            self.i += 1

    def literal_string(self) -> str:
        end = self.s.find("'", self.i + 1)
        if end < 0 or "\n" in self.s[self.i : end]:
            raise self.error("unterminated string")
        text = self.s[self.i + 1 : end]
        self.i = end + 1
        return text

    def multiline_literal(self) -> str:
        self.i += 3
        if self.peek("\n"):
            self.i += 1
        end = self.s.find("'''", self.i)
        if end < 0:
            raise self.error("unterminated string")
        # Up to two quotes may end the content just before the close.
        extra = 0
        while extra < 2 and self.s.startswith("'", end + 3 + extra):
            extra += 1
        text = self.s[self.i : end + extra]
        self.i = end + 3 + extra
        return text

    def escape(self) -> str:
        code = self.s[self.i + 1 : self.i + 2]
        if code in _ESCAPES:
            self.i += 2
            return _ESCAPES[code]
        if code in ("u", "U"):
            width = 4 if code == "u" else 8
            digits = self.s[self.i + 2 : self.i + 2 + width]
            if not re.fullmatch(r"[0-9A-Fa-f]{%d}" % width, digits):
                raise self.error("a bad \\%s escape" % code)
            self.i += 2 + width
            return chr(int(digits, 16))
        raise self.error("an unknown escape \\%s" % code)

    # -- white space ----------------------------------------------------

    def peek(self, text: str) -> bool:
        return self.s.startswith(text, self.i)

    def spaces(self) -> None:
        while self.i < len(self.s) and self.s[self.i] in " \t":
            self.i += 1

    def comment(self) -> None:
        if self.peek("#"):
            end = self.s.find("\n", self.i)
            self.i = len(self.s) if end < 0 else end

    def blank_lines(self) -> None:
        while True:
            self.spaces()
            self.comment()
            if self.peek("\n"):
                self.i += 1
                continue
            return

    def end_of_line(self) -> None:
        self.spaces()
        self.comment()
        if self.i < len(self.s):
            if not self.peek("\n"):
                raise self.error("a newline was expected")
            self.i += 1

    def expect(self, text: str) -> None:
        if not self.peek(text):
            raise self.error("%r was expected" % text)
        self.i += len(text)

    def error(self, message: str) -> TomlError:
        line = self.s.count("\n", 0, self.i) + 1
        return TomlError("line %d: %s" % (line, message))


def number(text: str) -> Any:
    """An integer or a float from its TOML spelling."""
    clean = text.replace("_", "")
    sign = -1 if clean.startswith("-") else 1
    body = clean.lstrip("+-")
    if body in ("inf", "nan"):
        return float(clean)
    for prefix, base in (("0x", 16), ("0o", 8), ("0b", 2)):
        if body.startswith(prefix):
            return sign * int(body[2:], base)
    if any(c in body for c in ".eE"):
        return float(clean)
    if len(body) > 1 and body.startswith("0"):
        raise TomlError("leading zeros in %s" % text)
    return int(clean)
