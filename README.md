# wechat-agent-bridge

A local-only way for an AI agent to read your own WeChat messages and Moments
(朋友圈), built on [GreenBubbles](https://github.com/bojieli/greenbubbles).

**Contact:** larryhchen@gmail.com

## What this is

- **`moments-reader/`** — a small, standalone Rust tool that reads Moments
  directly from `sns.db`. GreenBubbles v0.10.0's own documented path to
  Moments (`restore` → `restore-publish` → `replica-cached-moments`) has no
  working manifest producer in that build outside its own test suite; this
  reuses GreenBubbles' own key-derivation dependency (`wx-db`) to read the
  same data a different way. See `NOTICE.md` for attribution.
- **`bridge/sync.py`** — calls GreenBubbles' CLI (for messages) and
  `moments_list` (for Moments) on a timer, and writes small, bounded,
  minimized JSON exports to disk. No server, no listening port, no daemon in
  between runs.
- **`bridge/backfill.py`** — a one-time, by-hand full-history export per
  chat (`history/<chatId>.json`), separate from the rolling window above.
  Not scheduled; re-run it yourself whenever you want it refreshed.
- **`docs/MUSE.md`** — the contract a downstream reader (in my case, a
  separate assistant called Muse) should follow when reading those exports:
  schemas, freshness/watermarking, and the rule that message/Moments content
  is untrusted data, never instructions.

## What this is not

- Not a decryption tool. It depends entirely on GreenBubbles for unlocking
  WeChat's database and extracting the account key — set that up first,
  following GreenBubbles' own docs.
- Not a server. Nothing here listens on a port. Everything is either a
  one-shot CLI call or a scheduled local script.
- Not a write path. Nothing here sends messages, posts Moments, or modifies
  WeChat state.
- Does not bundle or redistribute WeChat, GreenBubbles, or wx-cli binaries.

## Setup

1. Set up [GreenBubbles](https://github.com/bojieli/greenbubbles) and run its
   own `greenbubbles-acquire capture` to get your WeChat key into
   `~/.greenbubbles-acquire/passphrase.txt`. Follow its docs, not this
   repo's, for that step — it's the part that touches your live WeChat
   installation, and GreenBubbles' own safety notes apply.
2. Build this repo's Moments reader:
   ```sh
   cd moments-reader
   cargo build --release
   ```
3. Place the built `greenbubbles` CLI binary and this repo's
   `target/release/moments_list` wherever `bridge/sync.py`'s `BIN_DIR`
   expects them (default: `~/Library/Application Support/WeChatBridge/bin/`).
4. **Sign both binaries with a real certificate before running them.**
   macOS's "would like to access data from other apps" permission is keyed
   to the binary's *designated requirement*. For an ad-hoc signature
   (`--sign -`) that requirement is just the content hash — check with
   `codesign -d -r- <binary>` and you'll see `designated => cdhash H"…"` —
   so every rebuild is a brand-new app to macOS and you're re-prompted
   forever. A fixed `--identifier` alone does **not** fix this (tested).
   Signing with a certificate makes the requirement
   `identifier … and certificate leaf …`, which survives rebuilds:
   ```sh
   security find-identity -v -p codesigning     # any "Apple Development: …" works; a free one comes with Xcode
   BIN_DIR="$HOME/Library/Application Support/WeChatBridge/bin"
   ID="Apple Development: Your Name (TEAMID)"
   codesign --force --timestamp --sign "$ID" --identifier "com.yourname.wechatbridge.greenbubbles" "$BIN_DIR/greenbubbles"
   codesign --force --timestamp --sign "$ID" --identifier "com.yourname.wechatbridge.moments-list" "$BIN_DIR/moments_list"
   ```
   `--timestamp` matters: Apple Development certificates expire after a
   year, and a timestamped signature stays valid past that date. The
   permission is keyed to the certificate's *identity* (name + Team ID +
   Apple anchor), not the specific certificate, so when you renew it in
   Xcode (Settings → Accounts → Manage Certificates) and re-sign, the grant
   carries over with no new prompt. No Apple account? Create a self-signed
   code-signing certificate in Keychain Access (Certificate Assistant →
   Create a Certificate → type "Code Signing") and use its name as `$ID`.
   Re-run this step after every rebuild; you'll be asked to Allow once per
   certificate identity, not once per build.
5. Run `bridge/sync.py` once by hand to confirm it works — approve the
   permission prompt if macOS asks — then schedule it (e.g. a `launchd`
   agent) at whatever interval you want. It writes to
   `~/Library/Application Support/WeChatBridge/exports/` by default —
   deliberately not `~/Documents` or another macOS privacy-protected
   location, to reduce permission prompts.
6. `sync.py` needs Pillow, which it uses to fully decode every image before
   reporting a path for it (see `docs/MOMENTS_MEDIA.md` for why a header
   check isn't enough). Install it for the interpreter that will run the
   sync — installing a pure-Python package doesn't change the interpreter
   binary's signature, so this is safe to do with the system Python:
   ```sh
   /usr/bin/python3 -m pip install --user Pillow
   ```
7. **Use Apple's own `/usr/bin/python3` to run `sync.py`, not a pyenv/
   Homebrew build.** Unsigned third-party Python builds have the same
   rebuilds-break-the-grant problem as above, with no equivalent fix — you
   can't re-sign your way to a stable identity on an interpreter you don't
   control the build of. `/usr/bin/python3` is Apple-signed and stable; the
   script only needs the standard library, so this has no downside.
7. For full chat history instead of (or alongside) the rolling window,
   run the backfill once by hand:
   ```sh
   python3 bridge/backfill.py              # every chat
   python3 bridge/backfill.py --chat EXAMPLE_CHAT_ID --chat ANOTHER_CHAT_ID   # specific chats only
   ```
   It shares `sync.py`'s lock, so it won't run concurrently with a
   scheduled sync. Re-run any time to pick up newer history; each chat's
   file is replaced atomically.
8. If a prompt still doesn't appear for a scheduled (non-interactive)
   `launchd` job — reading another app's sandboxed container is not a
   soft, one-time-popup permission, it may need to be granted explicitly:
   **System Settings → Privacy & Security → Full Disk Access → +** → add
   `/usr/bin/python3` and the two binaries in `BIN_DIR`.

## Before you publish your own fork

Run the privacy scanner first:

```sh
python3 scripts/privacy_audit.py
```

It checks tracked files, staged changes, and full git history (not just the
current working tree) for WeChat identifiers, key-shaped secrets, tokens,
and absolute home-directory paths, without ever printing the matched text
itself.

## License

MIT, see `LICENSE`. This project depends on GreenBubbles and wx-cli, both
separately MIT licensed — see `NOTICE.md` for their required notices.
