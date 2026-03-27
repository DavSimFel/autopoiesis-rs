#!/usr/bin/env bash
set -euo pipefail

scan_staged_file() {
    local path="$1"
    local content

    if ! content="$(git show ":$path" 2>/dev/null)"; then
        return 0
    fi

    if printf '%s' "$content" | grep -Eq 'ghp_[a-zA-Z0-9]{36}|AKIA[0-9A-Z]{16}|PRIVATE KEY'; then
        printf '[pre-commit] FAILED: possible secret detected in %s\n' "$path" >&2
        return 1
    fi

    if python3 - "$path" 3<<<"$content" <<'PY'
import re
import os
import sys

path = sys.argv[1]
text = os.fdopen(3, encoding="utf-8", errors="replace").read()

if re.search(r'ghp_[A-Za-z0-9]{36}|AKIA[0-9A-Z]{16}|PRIVATE KEY', text):
    sys.exit(1)

def strip_rust_syntax(source: str) -> str:
    out = []
    i = 0
    n = len(source)
    state = "code"
    raw_hashes = 0
    block_depth = 0
    while i < n:
        ch = source[i]
        nxt = source[i + 1] if i + 1 < n else ""
        if state == "code":
            if ch == "/" and nxt == "/":
                out.extend("  ")
                i += 2
                state = "line_comment"
                continue
            if ch == "/" and nxt == "*":
                out.extend("  ")
                i += 2
                state = "block_comment"
                block_depth = 1
                continue
            if ch == "r":
                j = i + 1
                while j < n and source[j] == "#":
                    j += 1
                if j < n and source[j] == "\"":
                    out.extend(" " * (j - i + 1))
                    raw_hashes = j - i - 1
                    i = j + 1
                    state = "raw_string"
                    continue
            if ch == '"':
                out.append(" ")
                i += 1
                state = "string"
                continue
            if ch == "'" and starts_char_literal(source, i):
                out.append(" ")
                i += 1
                state = "char"
                continue
            out.append(ch)
            i += 1
            continue
        if state == "line_comment":
            out.append("\n" if ch == "\n" else " ")
            if ch == "\n":
                state = "code"
            i += 1
            continue
        if state == "block_comment":
            out.append("\n" if ch == "\n" else " ")
            if ch == "/" and nxt == "*":
                block_depth += 1
                out.append(" ")
                i += 2
                continue
            if ch == "*" and nxt == "/":
                block_depth -= 1
                out.append(" ")
                i += 2
                if block_depth == 0:
                    state = "code"
                continue
            i += 1
            continue
        if state == "string":
            out.append("\n" if ch == "\n" else " ")
            if ch == "\\" and i + 1 < n:
                out.append("\n" if source[i + 1] == "\n" else " ")
                i += 2
                continue
            if ch == '"':
                state = "code"
            i += 1
            continue
        if state == "char":
            out.append("\n" if ch == "\n" else " ")
            if ch == "\\" and i + 1 < n:
                out.append("\n" if source[i + 1] == "\n" else " ")
                i += 2
                continue
            if ch == "'":
                state = "code"
            i += 1
            continue
        if state == "raw_string":
            out.append("\n" if ch == "\n" else " ")
            if ch == '"':
                closing = "#" * raw_hashes
                if source.startswith(closing, i + 1):
                    out.extend(" " * raw_hashes)
                    i += 1 + raw_hashes
                    state = "code"
                    continue
            i += 1
            continue
    return "".join(out)

def starts_char_literal(source: str, index: int) -> bool:
    if source[index] != "'":
        return False
    j = index + 1
    if j >= len(source):
        return False
    if source[j] == "\\":
        j += 1
        if j >= len(source):
            return False
        if source[j] == "u":
            j += 1
            if j >= len(source) or source[j] != "{":
                return False
            j += 1
            while j < len(source) and source[j] in "0123456789abcdefABCDEF_":
                j += 1
            if j >= len(source) or source[j] != "}":
                return False
            j += 1
        else:
            j += 1
    else:
        j += 1
    return j < len(source) and source[j] == "'"

stripped = strip_rust_syntax(text)

def is_test_only_rust_path(path: str) -> bool:
    return path.endswith(".rs") and (
        path.startswith("tests/")
        or "/tests/" in path
        or path.endswith("/tests.rs")
        or path.endswith("/tests/mod.rs")
    )

if is_test_only_rust_path(path):
    sys.exit(0)

test_ranges = []
search_from = 0
marker = "#[cfg(test)]"
while True:
    start = stripped.find(marker, search_from)
    if start == -1:
        break
    brace = stripped.find("{", start + len(marker))
    if brace == -1:
        search_from = start + len(marker)
        continue
    depth = 1
    end = brace + 1
    while end < len(stripped) and depth > 0:
        if stripped[end] == "{":
            depth += 1
        elif stripped[end] == "}":
            depth -= 1
        end += 1
    test_ranges.append((brace, end))
    search_from = end

def in_test_range(index: int) -> bool:
    return any(start <= index < end for start, end in test_ranges)

for match in re.finditer(r"sk-[A-Za-z0-9_-]{20,}", text):
    if not in_test_range(match.start()):
        sys.exit(1)

sys.exit(0)
PY
    then
        return 0
    fi

    printf '[pre-commit] FAILED: possible secret detected in %s\n' "$path" >&2
    return 1
}

main() {
    local files
    mapfile -t files < <(git diff --cached --name-only --diff-filter=ACM)

    for path in "${files[@]}"; do
        scan_staged_file "$path"
    done
}

main "$@"
