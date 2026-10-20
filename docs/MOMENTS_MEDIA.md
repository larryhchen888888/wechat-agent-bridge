# Moments media: why the CDN URL doesn't work, and what does

`SnsTimeLine.content` for a Moments post is XML. Each photo's `<media>`
element looks like:

```xml
<media><id>...</id><type>2</type>
  <url type="1" key="..." enc_idx="1" token="...">http://mmsns.c2c.wechat.com/mmsns/.../0</url>
  <size width="281" height="345" totalSize="42031"/>
</media>
```

Fetching that `url` directly returns `HTTP 400`, from a server identifying
itself as `ImgHttp3.0.0`, with header `X-ErrNo: -106`. The `key`/`enc_idx`/
`token` attributes on `<url>` are real and clearly auth-related, but
appending them as query parameters (`?token=...`, `?wxtoken=...`), or
setting a WeChat-like `User-Agent`, still returns 400. This is Tencent's
proprietary signed-request scheme for its image CDN — not a missing query
parameter. Treat it as out of scope: implementing it means reverse
engineering a private API, not reading data already on your own disk.

## What actually works: WeChat's own local cache

WeChat caches Moments photos you've viewed, locally, under:

```
<account>/cache/<YYYY-MM>/Sns/Img/<2-char>/<rest>
```

encrypted with the same local "V2" `.dat` scheme GreenBubbles already
decodes for chat images — `wx_media::detect_dat_format` /
`wx_media::decrypt_dat`, keyed by `wx_media::derive_v2_key_from_dir(account_root)`.
That key is derived from files already in the account directory; it does
not involve the WeChat database passphrase or any network request.

The remaining problem is matching a specific post's `<media>` entry to its
cached file. The cache filename is not an MD5 of anything tested: the URL
(plain, https, without the size suffix, with WeChat's `?tp=wxpc&length=N&
width=N` fetch parameters at eight sizes), the thumbnail URL, the media
`id`, the post `id`, the `tid`, the `key`/`enc_idx` attributes, nor
SHA-1/SHA-256 prefixes of the URL — zero hits across 3,750 photo entries
and 5,132 cached files. `hardlink.db` (which indexes chat attachments by
MD5) doesn't cover this cache either.

What's left is the post's own `<size width= height= totalSize=>`.
`--resolve-media` decrypts every file in the post's month (±1) and matches
on **all three**: exact decoded byte length, then exact pixel dimensions
read from the decoded header, and the match must be **unique**. Any
ambiguity yields `null`.

**Byte size alone is not enough, and a first version shipped with only
that.** Thousands of small cached previews share a narrow size range, so
"a file of exactly N bytes" is frequently some other post's photo — it put
one person's photo under another person's post before the dimension check
existed. Adding dimensions cut the resolved count from 17 to 3 on the same
500 posts: the 14 that disappeared were false matches, not lost ones.
`bridge/sync.py` re-checks the decoded dimensions against the post
independently and reports anything it has to drop as
`misattributedMomentsMediaDropped`.

Why so few survive: the post declares the *original's* size and
dimensions, but what WeChat caches while you scroll is a downscaled
preview, which can never match. In practice the photos that resolve are
the account holder's own posts (WeChat keeps those originals locally) and
images whose original is already tiny. That is the real ceiling of this
approach without reverse-engineering the client's cache naming.

**Heuristic attribution of previews doesn't work either — tested.** When
a multi-photo post scrolls into view, macOS WeChat fetches its grid
thumbnails as a burst (six files within one second, observed). Tempting
to attribute that burst to a post by photo count + aspect ratio + month.
But every thumbnail in the burst was 200×267, and no post in that month
with six photos had six 3:4 photos (best was 4 of 6): the client renders
grid thumbnails into a fixed 200×267 cell regardless of the source shape,
so a thumbnail carries no attributable information. Photo count alone
left 14 candidate posts. Don't ship a guess.

## The decrypt bug that made every photo look fine and be broken

The first version of this shipped files that `file`, `sips`, and a lenient
viewer all accepted — correct header, correct dimensions, correct byte
length — and that were nonetheless corrupt from about 1 KB onward (a strict
decoder like Pillow's `Image.load()` fails with "broken data stream"; a
lenient one renders the first few scan lines and gray-fills the rest).

Cause: WeChat's V2 `.dat` layout is an AES-128-ECB section followed by a
tail XOR'd with a single **per-account** byte. `wx_media::decrypt_dat`
handles the tail — but when called with `xor_key: None` it falls back to a
hardcoded `0x37` (`dat.rs:162`), which is simply the wrong byte on accounts
whose key differs (this one's is `0xB3`). XOR preserves length, so nothing
about the file's size or header betrays it. GreenBubbles itself makes the
same call with `xor_key: None` in `live_attachment.rs` and `artifact.rs`,
so its own chat-image decode has the identical defect on such accounts —
confirmed here by running its output through a full decode.

Fix (in `main.rs`, `detect_sns_xor_key`): the key is readable off any
cached JPEG's trailer, since the last two raw bytes are `FF D9 ^ key`.
Majority-vote across the newest months' cache files — the same technique
as the library's own `detect_xor_key`, which only looks at chat thumbnails
named `*_t.dat` and so never sees the Sns cache — then pass the result in.
If no key can be determined, media resolution is disabled outright rather
than producing plausible-looking corrupt files.

Lesson that's now enforced in `bridge/sync.py`: a `localPath` or chat
`file` is only reported after a **full** decode succeeds. Header-only
checks are not verification.

## What this does and doesn't give you

- A photo resolves to `localPath` only if it was actually viewed in the
  WeChat app at some point — there's no way to retroactively cache
  something nobody looked at, short of the CDN protocol above.
- There's no network request anywhere in this path. Everything is a local
  file read plus a local decrypt.
- Videos (`<media><type>6</type>`) live under `Sns/Video/` as **plain
  MP4** — no `.dat` wrapper, no decryption. Matching is by file size
  against the post's `totalSize`, same as photos. The cache filename is
  *not* the XML's `md5`, `videomd5`, nor an MD5 of the URL, its
  `encfilekey`, or the thumbnail URL (all tested against 94 cached files:
  zero matches), so size is the only key available. Coverage is poor for
  recent posts: WeChat now caches a server-transcoded copy (`dotrans=9` in
  the URL) whose size differs from the declared one, so 0 of the newest
  100 video posts matched while older posts do. Expect `localPath: null`
  for most recent videos; that's the cache, not a bug to work around.
- Media `<type>` values other than 2 and 6 (seen: 0, 4, a handful of 3)
  are reported as `kind: "other"` and never resolved.
- Opening a photo full-screen in macOS WeChat (4.1.13) does **not** cache
  the original. Tested directly: a full-screen view of another person's
  July post wrote exactly six 200×267 grid thumbnails to `Sns/Img` and
  nothing else anywhere in the container — no original in `Sns`, `temp`,
  `Caches`, or the shared area. The full-size view is served from memory
  or the network. Earlier versions of this document (and of the Muse
  contract) said the opposite; that was an assumption, not an observation.
  Consequence: for other people's posts there is no user action that makes
  a photo resolvable through this cache.
