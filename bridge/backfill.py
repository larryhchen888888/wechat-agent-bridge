#!/usr/bin/env python3
"""One-time full-history backfill.

Separate from sync.py and its 15-minute schedule on purpose: paging every
chat's complete history is much heavier than the rolling 2-day window, and
doing it on a timer would mean re-fetching everything repeatedly for no
reason. Run this by hand, once, whenever you want it -- re-running is safe
(each chat's file is replaced atomically) but pointless unless you want to
pick up messages WeChat has newly synced to this Mac since the last run.

Writes one file per chat to EXPORT_DIR/history/<chatId>.json, independent
of and in addition to the rolling messages.json sync.py keeps writing. A
chat with no messages WeChat has synced to this Mac gets no file.

Usage: python3 backfill.py [--chat <conversation-id> ...]
  With no --chat, backfills every conversation. Repeat --chat to backfill
  only specific ones (e.g. after adding a new chat you care about).
"""
from __future__ import annotations

import fcntl
import re
import sys
import time
from pathlib import Path

import sync  # bridge/sync.py, same directory

HISTORY_DIR = sync.EXPORT_DIR / "history"
PAGE_LIMIT = 500


def safe_filename(chat_id: str) -> str:
    return re.sub(r"[^A-Za-z0-9_-]", "_", chat_id) + ".json"


def fetch_full_history(conversation_id: str) -> list[dict]:
    """Pages a single conversation from newest to oldest until exhausted."""
    messages: list[dict] = []
    cursor: str | None = None
    while True:
        args = ["messages", "list", "--conversation", conversation_id, "--limit", str(PAGE_LIMIT)]
        if cursor:
            args += ["--cursor", cursor]
        header, page_messages = sync.run_json_lines(args)
        messages.extend(page_messages)
        if not header.get("hasMore"):
            break
        cursor = header.get("nextCursor")
        if not cursor:
            break
    return messages


def backfill_chat(conversation_id: str) -> dict:
    messages = fetch_full_history(conversation_id)
    chat_record = {"chatId": conversation_id, "messages": messages}
    dropped = sync.drop_invalid_media([chat_record])
    now = int(time.time())
    sync.atomic_write(
        HISTORY_DIR / safe_filename(conversation_id),
        {
            "chatId": conversation_id,
            "generatedAtUnix": now,
            "messageCount": len(messages),
            "messages": messages,
        },
    )
    return {"chatId": conversation_id, "messageCount": len(messages), "invalidImagesDropped": dropped}


def main() -> int:
    HISTORY_DIR.mkdir(parents=True, exist_ok=True)
    import os

    os.chmod(HISTORY_DIR, 0o700)

    # Share sync.py's lock so this never runs concurrently with the
    # scheduled sync -- both would contend for the same decode cache.
    lock_handle = open(sync.LOCK_FILE, "w")  # noqa: SIM115
    try:
        fcntl.flock(lock_handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        print("a sync or backfill is already running; try again shortly", file=sys.stderr)
        return 1

    if not sync.PASSPHRASE_FILE.is_file():
        print("passphrase file missing; re-run greenbubbles-acquire capture", file=sys.stderr)
        return 1

    requested = sys.argv[1:]
    chat_ids: list[str]
    if requested and requested[0] == "--chat":
        chat_ids = [value for flag, value in zip(requested[::2], requested[1::2]) if flag == "--chat"]
    else:
        conversations = sync.list_conversations()
        chat_ids = [item["id"] for item in conversations]

    print(f"backfilling {len(chat_ids)} chat(s)...")
    results = []
    total_messages = 0
    total_dropped = 0
    for index, chat_id in enumerate(chat_ids, start=1):
        try:
            result = backfill_chat(chat_id)
        except Exception as error:  # noqa: BLE001
            print(f"  [{index}/{len(chat_ids)}] {chat_id}: FAILED -- {error}", file=sys.stderr)
            continue
        total_messages += result["messageCount"]
        total_dropped += result["invalidImagesDropped"]
        print(f"  [{index}/{len(chat_ids)}] {chat_id}: {result['messageCount']} messages")
        results.append(result)

    now = int(time.time())
    sync.atomic_write(
        sync.EXPORT_DIR / "backfill_status.json",
        {
            "completedAtUnix": now,
            "chatsBackfilled": len(results),
            "chatsRequested": len(chat_ids),
            "totalMessages": total_messages,
            "invalidImagesDropped": total_dropped,
        },
    )
    print(f"done: {len(results)}/{len(chat_ids)} chats, {total_messages} messages total")
    return 0


if __name__ == "__main__":
    sys.exit(main())
