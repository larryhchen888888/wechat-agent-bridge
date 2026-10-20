#!/usr/bin/env python3
"""WeChat bridge sync.

Reads an already-captured WeChat key, pulls recent messages and Moments
through GreenBubbles' own live reader plus moments-reader (this project's
Moments tool), and writes bounded, minimized JSON exports to EXPORT_DIR.

No server, no daemon in between runs. Nothing is written except these
export files; decrypted content never leaves this process.

Prerequisites (not provided by this repo):
  - GreenBubbles built and its WeChat key already captured via its own
    greenbubbles-acquire tool, at PASSPHRASE_FILE.
  - The `greenbubbles` binary and this repo's `moments_list` binary placed
    at the paths BIN_DIR expects (see README.md).
"""
from __future__ import annotations

import fcntl
import json
import os
import subprocess
import sys
import time
from pathlib import Path

from PIL import Image

HOME = Path.home()
BIN_DIR = HOME / "Library" / "Application Support" / "WeChatBridge" / "bin"
EXPORT_DIR = HOME / "Library" / "Application Support" / "WeChatBridge" / "exports"
PASSPHRASE_FILE = HOME / ".greenbubbles-acquire" / "passphrase.txt"

GREENBUBBLES = BIN_DIR / "greenbubbles"
MOMENTS_LIST = BIN_DIR / "moments_list"

MESSAGE_WINDOW_DAYS = 2
MOMENTS_WINDOW_DAYS = 7
MOMENTS_FETCH_LIMIT = 500  # over-fetch, then filter by date
MEDIA_CACHE_DIR = BIN_DIR.parent / "media-cache"
LOCK_FILE = BIN_DIR.parent / "sync.lock"
# A single `messages list` page decrypts every image attachment it
# returns, so the first pass over a busy group chat after GreenBubbles'
# decode cache has been cleared can legitimately take well over a minute.
# The job runs every 15 minutes; a generous ceiling costs nothing.
COMMAND_TIMEOUT_SECONDS = 240


def find_db_storage_root() -> Path | None:
    base = HOME / "Library" / "Containers" / "com.tencent.xinWeChat" / "Data" / "Documents" / "xwechat_files"
    if not base.is_dir():
        return None
    for child in base.iterdir():
        candidate = child / "db_storage"
        if candidate.is_dir() and (candidate / "message").is_dir():
            return candidate
    return None


def run_json_lines(args: list[str]) -> tuple[dict, list[dict]]:
    """Run a greenbubbles command, parse its JSON-Lines brief output."""
    result = subprocess.run(
        [str(GREENBUBBLES), *args],
        capture_output=True,
        text=True,
        timeout=COMMAND_TIMEOUT_SECONDS,
    )
    if result.returncode != 0:
        raise RuntimeError(f"{' '.join(args)} failed: {result.stderr.strip()}")
    lines = [line for line in result.stdout.splitlines() if line.strip()]
    if not lines:
        return {}, []
    header = json.loads(lines[0])
    items = [json.loads(line) for line in lines[1:]]
    return header, items


def list_conversations() -> list[dict]:
    conversations = []
    cursor = None
    while True:
        args = ["conversations", "list", "--limit", "500"]
        if cursor:
            args += ["--cursor", cursor]
        result = subprocess.run(
            [str(GREENBUBBLES), *args], capture_output=True, text=True, timeout=COMMAND_TIMEOUT_SECONDS
        )
        if result.returncode != 0:
            raise RuntimeError(f"conversations list failed: {result.stderr.strip()}")
        page = json.loads(result.stdout)
        conversations.extend(page.get("items", []))
        page_info = page.get("page", {})
        if not page_info.get("hasMore"):
            break
        cursor = page_info.get("nextCursor")
        if not cursor:
            break
    return conversations


def fetch_recent_messages(conversation_ids: list[str], since_unix: int) -> list[dict]:
    chats = []
    for conversation_id in conversation_ids:
        header, messages = run_json_lines(
            [
                "messages",
                "list",
                "--conversation",
                conversation_id,
                "--since",
                str(since_unix),
                "--limit",
                "500",
            ]
        )
        if messages:
            chats.append({"chatId": conversation_id, "messages": messages})
    return chats


def fetch_recent_moments(db_root: Path, since_unix: int) -> list[dict]:
    with open(PASSPHRASE_FILE) as handle:
        key_hex = handle.read().strip()
    result = subprocess.run(
        [str(MOMENTS_LIST), str(db_root), "--limit", str(MOMENTS_FETCH_LIMIT),
         "--since", str(since_unix),
         "--resolve-media", "--media-cache-dir", str(MEDIA_CACHE_DIR)],
        input=key_hex,
        capture_output=True,
        text=True,
        timeout=COMMAND_TIMEOUT_SECONDS,
    )
    if result.returncode != 0:
        raise RuntimeError(f"moments_list failed: {result.stderr.strip()}")
    moments = []
    for line in result.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        moment = json.loads(line)
        if moment.get("createdAtUnix", 0) >= since_unix:
            moments.append(moment)
    return moments


IMAGE_SUFFIXES = {".jpg", ".jpeg", ".png", ".gif", ".heic", ".webp", ".bmp"}


def is_valid_image(path: str | None) -> bool:
    """Fully decodes the image, not just its header.

    A bug in the shared decrypt library this project (and GreenBubbles'
    own chat-image decode) relies on can produce JPEGs whose declared byte
    length matches the source, but whose compressed scan data is corrupted
    partway through -- `file`/`sips`/a lenient viewer can still report a
    plausible-looking header and dimensions for these. Only a full decode
    catches it.
    """
    if not path:
        return False
    try:
        with Image.open(path) as image:
            image.load()
        return True
    except Exception:  # noqa: BLE001
        return False


def drop_invalid_media(chats_with_messages: list[dict]) -> int:
    """Only validates image attachments -- a `file` field can also be a
    voice, video, or document path, and those are out of scope for this
    check (PIL can't open them, and that's not evidence of corruption)."""
    dropped = 0
    for chat in chats_with_messages:
        for message in chat["messages"]:
            file_path = message.get("file")
            if not file_path or Path(file_path).suffix.lower() not in IMAGE_SUFFIXES:
                continue
            if not is_valid_image(file_path):
                del message["file"]
                dropped += 1
    return dropped


def is_valid_mp4(path: str | None) -> bool:
    """Structural check only: an `ftyp` box up front and a `moov` box
    somewhere -- a partial download typically lacks the latter."""
    if not path:
        return False
    try:
        with open(path, "rb") as handle:
            data = handle.read()
    except OSError:
        return False
    return len(data) > 12 and data[4:8] == b"ftyp" and b"moov" in data


def decoded_image_size(path: str) -> tuple[int, int] | None:
    try:
        with Image.open(path) as image:
            image.load()
            return image.size
    except Exception:  # noqa: BLE001
        return None


def drop_invalid_moments_media(moments: list[dict]) -> tuple[int, int]:
    """Returns (undecodable dropped, misattributed dropped).

    The second check is independent of moments_list's own: the decoded
    image's real dimensions must equal the post's declared width/height.
    A file that decodes fine but is the wrong size is someone else's photo
    -- that happened, and a wrongly attributed photo is worse than none."""
    undecodable = misattributed = 0
    for moment in moments:
        for item in moment.get("media", []):
            local_path = item.get("localPath")
            if not local_path:
                continue
            if Path(local_path).suffix.lower() == ".mp4":
                if not is_valid_mp4(local_path):
                    item["localPath"] = None
                    undecodable += 1
                continue
            size = decoded_image_size(local_path)
            if size is None:
                item["localPath"] = None
                undecodable += 1
                continue
            declared = (item.get("declaredWidth"), item.get("declaredHeight"))
            if all(declared) and tuple(declared) != tuple(size):
                item["localPath"] = None
                misattributed += 1
    return undecodable, misattributed


def prune_media_cache(moments: list[dict]) -> int:
    """Media files are content-addressed and never overwritten, so without
    this the cache grows forever as the window slides. Keep only what the
    export just written still points at."""
    referenced = {
        item["localPath"]
        for moment in moments
        for item in moment.get("media", [])
        if item.get("localPath")
    }
    pruned = 0
    if MEDIA_CACHE_DIR.is_dir():
        for entry in MEDIA_CACHE_DIR.iterdir():
            if entry.is_file() and str(entry) not in referenced:
                entry.unlink()
                pruned += 1
    return pruned


def atomic_write(path: Path, data: dict) -> None:
    tmp_path = path.with_suffix(path.suffix + ".tmp")
    with open(tmp_path, "w") as handle:
        json.dump(data, handle, ensure_ascii=False, indent=2)
    os.chmod(tmp_path, 0o600)
    tmp_path.replace(path)


def main() -> int:
    EXPORT_DIR.mkdir(parents=True, exist_ok=True)
    os.chmod(EXPORT_DIR, 0o700)

    # One sync at a time: a manual run overlapping the scheduled one makes
    # both contend for the same database and decode cache, and one of them
    # times out. The lock is released when the process exits.
    lock_handle = open(LOCK_FILE, "w")  # noqa: SIM115 -- held for the process lifetime
    try:
        fcntl.flock(lock_handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        print("another sync is already running; skipping this run", file=sys.stderr)
        return 0

    now = int(time.time())
    status = {
        "ok": False,
        "syncedAtUnix": now,
        "dbPathsOk": False,
        "passphraseOk": False,
        "chatCount": 0,
        "messageCount": 0,
        "momentCount": 0,
        "invalidImagesDropped": 0,
        "invalidMomentsImagesDropped": 0,
        "misattributedMomentsMediaDropped": 0,
        "mediaCachePruned": 0,
        "error": None,
    }

    try:
        if not PASSPHRASE_FILE.is_file():
            raise RuntimeError("passphrase file missing; re-run greenbubbles-acquire capture")
        status["passphraseOk"] = True

        db_root = find_db_storage_root()
        if db_root is None:
            raise RuntimeError("could not locate WeChat db_storage directory")
        status["dbPathsOk"] = True

        conversations = list_conversations()
        status["chatCount"] = len(conversations)
        atomic_write(
            EXPORT_DIR / "chats.json",
            {"generatedAtUnix": now, "chats": conversations},
        )

        message_cutoff = now - MESSAGE_WINDOW_DAYS * 86400
        recent_conversation_ids = [
            item["id"]
            for item in conversations
            if item.get("sortTimestamp", 0) >= message_cutoff
        ]
        chats_with_messages = fetch_recent_messages(recent_conversation_ids, message_cutoff)
        status["invalidImagesDropped"] = drop_invalid_media(chats_with_messages)
        message_count = sum(len(chat["messages"]) for chat in chats_with_messages)
        status["messageCount"] = message_count
        atomic_write(
            EXPORT_DIR / "messages.json",
            {
                "generatedAtUnix": now,
                "windowDays": MESSAGE_WINDOW_DAYS,
                "chats": chats_with_messages,
            },
        )

        moments_cutoff = now - MOMENTS_WINDOW_DAYS * 86400
        moments = fetch_recent_moments(db_root, moments_cutoff)
        undecodable, misattributed = drop_invalid_moments_media(moments)
        status["invalidMomentsImagesDropped"] = undecodable
        status["misattributedMomentsMediaDropped"] = misattributed
        status["mediaCachePruned"] = prune_media_cache(moments)
        status["momentCount"] = len(moments)
        atomic_write(
            EXPORT_DIR / "moments.json",
            {
                "generatedAtUnix": now,
                "windowDays": MOMENTS_WINDOW_DAYS,
                "moments": moments,
            },
        )

        status["ok"] = True
    except Exception as error:  # noqa: BLE001
        status["error"] = str(error)

    atomic_write(EXPORT_DIR / "status.json", status)

    if not status["ok"]:
        print(f"wechat bridge sync failed: {status['error']}", file=sys.stderr)
        return 1
    print(
        f"synced: {status['chatCount']} chats, {status['messageCount']} messages, "
        f"{status['momentCount']} moments"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
