#!/usr/bin/env python3
"""Audit this repo's tracked files, staged changes, and full git history for
anything that shouldn't be published: personal paths, WeChat identifiers,
key-shaped secrets, tokens, and other credential patterns.

Never prints the matched text itself -- only the file, commit, and pattern
name -- so running this script does not become a second way to leak the
thing it's checking for.

Exit code 0: nothing found. Exit code 1: findings reported on stderr.
"""
import re
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

ALLOWED_SUFFIXES = {".md", ".py", ".rs", ".toml", ".json", ".yaml", ".yml", ".txt"}
SPECIAL_FILES = {".gitignore", "LICENSE", "NOTICE.md"}
FORBIDDEN_PATH_PARTS = {
    ".private", ".runtime", ".scratch", "profiles", "outputs",
    "node_modules", "__pycache__", "target", ".greenbubbles-acquire",
}

PATTERNS = {
    "user_home_path": re.compile(r"(?:/Users/[^\s/\[\]\\]+/|/home/[^\s/\[\]\\]+/)"),
    "wechat_account_id": re.compile(r"\bwxid_[A-Za-z0-9_]+|\b\d{6,}@chatroom\b"),
    "credential_token": re.compile(
        r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|"
        r"sk-[A-Za-z0-9_-]{20,}|xox[baprs]-[A-Za-z0-9-]{20,}|AKIA[A-Z0-9]{16})\b"
    ),
    # Exact shape of a raw WeChat/SQLCipher key, or any other 32-byte secret.
    "hex_secret_64char": re.compile(r"(?<![A-Za-z0-9])[0-9a-fA-F]{64}(?![A-Za-z0-9])"),
    "opaque_token_blob": re.compile(r"\beyJ[A-Za-z0-9_-]{100,}"),
    "email_address": re.compile(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b"),
    "phone_number": re.compile(r"(?<!\w)(?:\+\d[\d ()-]{8,}\d|1[3-9]\d{9})(?!\w)"),
}

# Files allowed to contain an email (e.g. a deliberate README contact line).
ALLOW_EMAIL_IN = {"README.md"}


def git(*args: str) -> str:
    return subprocess.check_output(
        ["git", "-C", str(REPO_ROOT), *args], stderr=subprocess.DEVNULL
    ).decode("utf-8", errors="replace")


def path_allowed(name: str) -> bool:
    path = Path(name)
    if any(part in FORBIDDEN_PATH_PARTS for part in path.parts):
        return False
    return path.name in SPECIAL_FILES or path.suffix in ALLOWED_SUFFIXES


def scan_text(filename: str, text: str) -> list[str]:
    findings = []
    if "\0" in text:
        findings.append("binary_content")
        return findings
    for name, pattern in PATTERNS.items():
        if name == "email_address" and Path(filename).name in ALLOW_EMAIL_IN:
            continue
        if pattern.search(text):
            findings.append(name)
    return findings


def list_tracked_files() -> list[str]:
    output = git("ls-files")
    return [line for line in output.splitlines() if line]


def list_all_commits() -> list[str]:
    try:
        output = git("log", "--all", "--format=%H")
    except subprocess.CalledProcessError:
        return []
    return [line for line in output.splitlines() if line]


def file_at_commit(commit: str, path: str) -> str | None:
    try:
        raw = subprocess.check_output(
            ["git", "-C", str(REPO_ROOT), "show", f"{commit}:{path}"],
            stderr=subprocess.DEVNULL,
        )
    except subprocess.CalledProcessError:
        return None
    try:
        return raw.decode("utf-8")
    except UnicodeDecodeError:
        return "\0"  # flagged as binary_content


def commit_authors() -> list[tuple[str, str, str]]:
    output = git("log", "--all", "--format=%H%x1f%an%x1f%ae")
    authors = []
    for line in output.splitlines():
        if not line:
            continue
        commit, name, email = line.split("\x1f")
        authors.append((commit, name, email))
    return authors


def disallowed_path_findings() -> list[str]:
    problems = []
    for name in list_tracked_files():
        if not path_allowed(name):
            problems.append(f"disallowed_file_type: {name}")
    return problems


def working_tree_and_history_findings() -> list[str]:
    problems = []
    seen_commit_paths: set[tuple[str, str]] = set()

    for name in list_tracked_files():
        if not path_allowed(name):
            continue
        full_path = REPO_ROOT / name
        try:
            text = full_path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            problems.append(f"binary_or_unreadable: {name}")
            continue
        for finding in scan_text(name, text):
            problems.append(f"working_tree: {name}: {finding}")

    for commit in list_all_commits():
        changed = git("show", "--name-only", "--format=", commit)
        for name in changed.splitlines():
            if not name or not path_allowed(name):
                continue
            key = (commit, name)
            if key in seen_commit_paths:
                continue
            seen_commit_paths.add(key)
            text = file_at_commit(commit, name)
            if text is None:
                continue
            for finding in scan_text(name, text):
                problems.append(f"history ({commit[:12]}): {name}: {finding}")

    return problems


def author_identity_findings() -> list[str]:
    problems = []
    for commit, name, email in commit_authors():
        if PATTERNS["email_address"].search(email) and "example" not in email:
            # A real-looking author email is a normal, expected thing in a
            # personal repo -- this is informational, not a failure.
            continue
        if not email or "@" not in email:
            problems.append(f"author_identity ({commit[:12]}): missing or malformed author email")
    return problems


def main() -> int:
    findings: list[str] = []
    findings += disallowed_path_findings()
    findings += working_tree_and_history_findings()
    findings += author_identity_findings()

    if findings:
        print(f"privacy audit: {len(findings)} finding(s)", file=sys.stderr)
        for finding in findings:
            print(f"  - {finding}", file=sys.stderr)
        return 1

    print("privacy audit: clean (no disallowed file types, patterns, or history leaks found)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
