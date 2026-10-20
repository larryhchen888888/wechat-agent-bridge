//! Minimal, read-only Moments (sns.db) reader.
//!
//! GreenBubbles (https://github.com/bojieli/greenbubbles, MIT, (c) Bojie Li)
//! has no working production path to its own documented Moments feature in
//! the v0.10.0 build this was verified against: `restore`/`restore-publish`
//! require an internal snapshot-manifest format that nothing in that build
//! actually produces outside its own test suite. This reads sns.db directly
//! instead, the same way GreenBubbles' own `live_query.rs` reads the message
//! databases: via `wx_db::open_readonly_connection` with the real WeChat
//! account key (wx-db / wx-decrypt: https://github.com/pandorafuture/wx-cli,
//! MIT, (c) pandorafuture). See NOTICE.md.
//!
//! This does not decrypt anything itself and does not discover or capture
//! the WeChat key — get that from GreenBubbles' own `greenbubbles-acquire`
//! first. See ../README.md.
//!
//! ## Media
//!
//! A Moments photo's `<media><url>` is a WeChat CDN path that returns HTTP
//! 400 when fetched directly — it needs a signed request this project does
//! not implement (see docs/MOMENTS_MEDIA.md). Instead, with
//! `--resolve-media --media-cache-dir <dir>`, this looks for the photo
//! already cached locally by WeChat's own app (under
//! `<account>/cache/<year>-<month>/Sns/Img/`), encrypted with the same
//! local "V2" scheme GreenBubbles already decodes for chat images
//! (`wx_media`), decrypts it, and writes the plaintext into `<dir>`,
//! content-addressed by its own SHA-256. `localPath` in the output always
//! points at that decoded file, never at the still-encrypted cache file.
//! A photo is only found this way if you actually viewed it in the app;
//! there is no network request involved either way.
//!
//! Usage: `cat <passphrase-file> | moments_list <db_storage_root> [--limit N] [--since <unix>] [--resolve-media --media-cache-dir <dir>]`

use roxmltree::{Document, ParsingOptions};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

struct MediaItem {
    url: String,
    total_size: Option<u64>,
    /// Declared `<size width= height=>`, used with byte size to identify
    /// the cached file. Size alone collides -- see `pick_unique_photo`.
    declared_dims: Option<(u32, u32)>,
    /// WeChat's `<media><type>`: 2 = photo, 6 = video (others left alone).
    media_type: Option<u32>,
}

const MEDIA_TYPE_PHOTO: u32 = 2;
const MEDIA_TYPE_VIDEO: u32 = 6;

struct Moment {
    tid: i64,
    author_username: String,
    created_at: i64,
    content_desc: String,
    media: Vec<MediaItem>,
}

fn parse_moment_xml(tid: i64, author_username: &str, xml: &str) -> Moment {
    let options = ParsingOptions {
        allow_dtd: false,
        ..ParsingOptions::default()
    };
    let mut content_desc = String::new();
    let mut created_at: i64 = 0;
    let mut media = Vec::new();

    if let Ok(doc) = Document::parse_with_options(xml, options) {
        for node in doc.descendants() {
            match node.tag_name().name() {
                "contentDesc" => {
                    if let Some(text) = node.text() {
                        content_desc = text.to_string();
                    }
                }
                "createTime" if node.parent().map(|p| p.tag_name().name()) == Some("TimelineObject") => {
                    if let Some(text) = node.text() {
                        created_at = text.trim().parse().unwrap_or(0);
                    }
                }
                "media" => {
                    let url = node
                        .descendants()
                        .find(|n| n.tag_name().name() == "url")
                        .and_then(|n| n.text())
                        .map(|t| t.trim().to_string())
                        .filter(|t| !t.is_empty());
                    let size_node = node.descendants().find(|n| n.tag_name().name() == "size");
                    let total_size = size_node
                        .and_then(|n| n.attribute("totalSize"))
                        .and_then(|v| v.parse().ok());
                    let declared_dims = size_node.and_then(|n| {
                        let w: u32 = n.attribute("width")?.parse().ok()?;
                        let h: u32 = n.attribute("height")?.parse().ok()?;
                        (w > 0 && h > 0).then_some((w, h))
                    });
                    let media_type = node
                        .children()
                        .find(|n| n.tag_name().name() == "type")
                        .and_then(|n| n.text())
                        .and_then(|v| v.trim().parse().ok());
                    if let Some(url) = url {
                        media.push(MediaItem { url, total_size, declared_dims, media_type });
                    }
                }
                _ => {}
            }
        }
    }

    Moment {
        tid,
        author_username: author_username.to_string(),
        created_at,
        content_desc,
        media,
    }
}

fn load_contact_names(connection: &rusqlite::Connection) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut statement = match connection
        .prepare("SELECT username, remark, nick_name, alias FROM contact")
    {
        Ok(s) => s,
        Err(_) => return map,
    };
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    });
    if let Ok(rows) = rows {
        for row in rows.flatten() {
            let (username, remark, nick_name, alias) = row;
            let display = remark
                .filter(|v| !v.is_empty())
                .or(nick_name.filter(|v| !v.is_empty()))
                .or(alias.filter(|v| !v.is_empty()));
            if let Some(display) = display {
                map.insert(username, display);
            }
        }
    }
    map
}

/// WeChat's V2 `.dat` layout is an AES-128 section followed by a tail XOR'd
/// with a single per-account byte. `wx_media::decrypt_dat` falls back to a
/// hardcoded guess (`0x37`) when no key is given, which silently corrupts
/// every image's tail on accounts where the real key differs -- the file
/// keeps the right length, header and dimensions, so header-only checks
/// (`file`, `sips`, lenient viewers) don't notice. The real key is readable
/// off any JPEG's trailer: the last two raw bytes are `FF D9` XOR key.
/// Majority-vote across the newest months' cached files, same approach as
/// the library's own `detect_xor_key`, which only looks at chat thumbnails.
fn detect_sns_xor_key(account_root: &Path) -> Option<u8> {
    let cache_dir = account_root.join("cache");
    let mut months: Vec<PathBuf> = std::fs::read_dir(&cache_dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("Sns").join("Img").is_dir())
        .collect();
    months.sort();
    months.reverse();

    let mut votes: HashMap<u8, usize> = HashMap::new();
    for month in months {
        let Ok(files) = walk(&month.join("Sns").join("Img")) else { continue };
        for path in files {
            let Ok(data) = std::fs::read(&path) else { continue };
            if data.len() < 16 || wx_media::detect_dat_format(&data) != Some(wx_media::DatFormat::V2) {
                continue;
            }
            let xor_size = u32::from_le_bytes([data[10], data[11], data[12], data[13]]) as usize;
            if xor_size < 2 {
                continue;
            }
            let candidate = data[data.len() - 2] ^ 0xFF;
            if data[data.len() - 1] ^ 0xD9 == candidate {
                *votes.entry(candidate).or_insert(0) += 1;
            }
        }
        if votes.values().copied().max().unwrap_or(0) >= 50 {
            break;
        }
    }
    votes.into_iter().max_by_key(|&(_, count)| count).map(|(key, _)| key)
}

/// Decrypts every cached image under one month's `Sns/Img` directory once,
/// indexed by decoded byte length -- keeping *every* file at each length.
/// Byte size alone is not an identity: thousands of small cached
/// thumbnails share a narrow size range, and an earlier version that kept
/// the first file per size handed one person's photo to another person's
/// post. Disambiguation happens in `pick_unique_photo`.
fn index_month_by_decoded_size(
    account_root: &Path,
    year_month: &str,
    key: &[u8; 16],
    xor_key: Option<u8>,
) -> HashMap<u64, Vec<PathBuf>> {
    let mut index: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    let img_dir = account_root
        .join("cache")
        .join(year_month)
        .join("Sns")
        .join("Img");
    let Ok(entries) = walk(&img_dir) else {
        return index;
    };
    let options = wx_media::DatDecryptOptions {
        v2_aes_key: Some(*key),
        xor_key,
    };
    for path in entries {
        let Ok(data) = std::fs::read(&path) else { continue };
        if let Ok(decoded) = wx_media::decrypt_dat(&data, &options) {
            index.entry(decoded.data.len() as u64).or_default().push(path);
        }
    }
    index
}

/// Pixel dimensions from the decoded bytes' own header (JPEG SOF, PNG
/// IHDR, GIF logical screen). Enough to compare against the post's
/// declared width/height; not a full decode -- that happens downstream.
fn image_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() >= 24 && data.starts_with(b"\x89PNG\r\n\x1a\n") {
        let w = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
        let h = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
        return Some((w, h));
    }
    if data.len() >= 10 && (data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a")) {
        return Some((
            u16::from_le_bytes([data[6], data[7]]) as u32,
            u16::from_le_bytes([data[8], data[9]]) as u32,
        ));
    }
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }
    let mut i = 2;
    while i + 4 <= data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = data[i + 1];
        if marker == 0xFF {
            i += 1;
            continue;
        }
        if marker == 0x01 || (0xD0..=0xD8).contains(&marker) {
            i += 2;
            continue;
        }
        let length = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        let is_sof = matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF);
        if is_sof {
            if i + 9 > data.len() {
                return None;
            }
            let h = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
            let w = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
            return Some((w, h));
        }
        if marker == 0xDA {
            return None;
        }
        i += 2 + length;
    }
    None
}

/// Among cache files whose decoded length equals the post's `totalSize`,
/// returns the one whose own pixel dimensions equal the post's declared
/// width/height -- and only if exactly one does. Two candidates that both
/// fit, or no declared dimensions to check against when more than one file
/// shares the size, both yield `None`: an unattributed photo is worse than
/// no photo.
fn pick_unique_photo(
    candidates: &[PathBuf],
    declared_dims: Option<(u32, u32)>,
    key: &[u8; 16],
    xor_key: Option<u8>,
) -> Option<PathBuf> {
    let options = wx_media::DatDecryptOptions {
        v2_aes_key: Some(*key),
        xor_key,
    };
    let mut fitting = Vec::new();
    for path in candidates {
        let Ok(data) = std::fs::read(path) else { continue };
        let Ok(decoded) = wx_media::decrypt_dat(&data, &options) else { continue };
        let dims = image_dimensions(&decoded.data);
        match (declared_dims, dims) {
            (Some(declared), Some(actual)) if declared == actual => fitting.push(path.clone()),
            (Some(_), _) => {}
            // No declared dimensions: size is all we have, so only a
            // single candidate can be trusted.
            (None, _) if candidates.len() == 1 => fitting.push(path.clone()),
            (None, _) => {}
        }
    }
    match fitting.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    }
}

/// Decrypts the one matched encrypted cache file and writes the plaintext
/// out to `media_cache_dir`, content-addressed by its own SHA-256. Returns
/// the decoded file's path -- never the still-encrypted source path.
fn materialize_decoded_image(
    encrypted_path: &Path,
    key: &[u8; 16],
    xor_key: Option<u8>,
    media_cache_dir: &Path,
) -> Option<PathBuf> {
    let data = std::fs::read(encrypted_path).ok()?;
    let options = wx_media::DatDecryptOptions {
        v2_aes_key: Some(*key),
        xor_key,
    };
    let decoded = wx_media::decrypt_dat(&data, &options).ok()?;
    std::fs::create_dir_all(media_cache_dir).ok()?;
    std::fs::set_permissions(media_cache_dir, std::fs::Permissions::from_mode(0o700)).ok()?;
    let digest = hex::encode(Sha256::digest(&decoded.data));
    let destination = media_cache_dir.join(format!("{digest}.{}", decoded.ext));
    if !destination.exists() {
        std::fs::write(&destination, &decoded.data).ok()?;
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o600)).ok()?;
    }
    Some(destination)
}

/// Cached Moments videos under `Sns/Video` are plain MP4 (no `.dat`
/// wrapping), so matching is by file size against the post's `totalSize`
/// and no decryption is involved. The filename is not the XML's `md5` or
/// `videomd5` (checked against this account: 0 of 94), so size is what we
/// have. Indexed per month like photos, keyed by byte length.
fn index_month_videos_by_size(account_root: &Path, year_month: &str) -> HashMap<u64, Vec<PathBuf>> {
    let mut index: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    let video_dir = account_root.join("cache").join(year_month).join("Sns").join("Video");
    let Ok(entries) = walk(&video_dir) else { return index };
    for path in entries {
        if path.extension().and_then(|e| e.to_str()) != Some("mp4") {
            continue;
        }
        if let Ok(meta) = std::fs::metadata(&path) {
            index.entry(meta.len()).or_default().push(path);
        }
    }
    index
}

/// Copies the matched plaintext MP4 out of WeChat's container (which a
/// downstream reader without Full Disk Access can't open) into
/// `media_cache_dir`, content-addressed like images. Checks for an MP4
/// `ftyp` box first so a partial download is never reported as a video.
fn materialize_video(source_path: &Path, media_cache_dir: &Path) -> Option<PathBuf> {
    let data = std::fs::read(source_path).ok()?;
    if data.len() < 12 || &data[4..8] != b"ftyp" {
        return None;
    }
    std::fs::create_dir_all(media_cache_dir).ok()?;
    std::fs::set_permissions(media_cache_dir, std::fs::Permissions::from_mode(0o700)).ok()?;
    let digest = hex::encode(Sha256::digest(&data));
    let destination = media_cache_dir.join(format!("{digest}.mp4"));
    if !destination.exists() {
        std::fs::write(&destination, &data).ok()?;
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o600)).ok()?;
    }
    Some(destination)
}

fn walk(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    Ok(out)
}

fn year_month_offset(unix_time: i64, offset_months: i64) -> String {
    const SECONDS_PER_DAY: i64 = 86400;
    let days = unix_time / SECONDS_PER_DAY;
    // Civil-from-days (Howard Hinnant's algorithm), proleptic Gregorian.
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };

    let mut total_month = year * 12 + (month - 1) + offset_months;
    let out_year = total_month.div_euclid(12);
    total_month = total_month.rem_euclid(12);
    format!("{out_year:04}-{:02}", total_month + 1)
}

fn main() {
    let mut key_hex = String::new();
    std::io::stdin().read_to_string(&mut key_hex).unwrap();
    let key_hex = key_hex.trim();
    let mut key = [0u8; 32];
    hex::decode_to_slice(key_hex, &mut key).expect("key must be 64 hex chars");

    let args: Vec<String> = std::env::args().collect();
    let db_storage_root = args.get(1).expect("usage: moments_list <db_storage_root> [--limit N] [--resolve-media]");
    let limit: usize = args
        .iter()
        .position(|a| a == "--limit")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let resolve_media = args.iter().any(|a| a == "--resolve-media");
    // Posts older than this are neither printed nor have media resolved --
    // resolving copies multi-megabyte videos out of WeChat's cache, so the
    // window matters for disk, not just output size.
    let since_unix: i64 = args
        .iter()
        .position(|a| a == "--since")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let media_cache_dir: Option<PathBuf> = args
        .iter()
        .position(|a| a == "--media-cache-dir")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    if resolve_media && media_cache_dir.is_none() {
        eprintln!("--resolve-media requires --media-cache-dir <path>");
        std::process::exit(1);
    }

    let sns_path = Path::new(db_storage_root).join("sns").join("sns.db");
    let contact_path = Path::new(db_storage_root).join("contact").join("contact.db");
    let account_root = Path::new(db_storage_root)
        .parent()
        .expect("db_storage_root must have a parent account directory");

    let sns_connection = wx_db::open_readonly_connection(&sns_path, Some(&key))
        .expect("could not open sns.db with the supplied key");
    let contact_connection = wx_db::open_readonly_connection(&contact_path, Some(&key))
        .expect("could not open contact.db with the supplied key");

    let names = load_contact_names(&contact_connection);

    let image_key = if resolve_media {
        wx_media::derive_v2_key_from_dir(account_root).ok()
    } else {
        None
    };
    // Without the real XOR key, decrypt_dat guesses and produces corrupt
    // tails that still look valid to header-only checks. Refuse to resolve
    // media at all rather than hand out files that only appear to work.
    let xor_key = if image_key.is_some() { detect_sns_xor_key(account_root) } else { None };
    if resolve_media && xor_key.is_none() {
        eprintln!("warning: could not determine the account's image XOR key; media will not be resolved");
    }
    let image_key = image_key.filter(|_| xor_key.is_some());
    // Cache one month's decoded-size index at a time; moments are read
    // newest-first, so consecutive rows usually share or neighbor a month.
    let mut month_index_cache: HashMap<String, HashMap<u64, Vec<PathBuf>>> = HashMap::new();
    let mut month_video_index_cache: HashMap<String, HashMap<u64, Vec<PathBuf>>> = HashMap::new();

    let mut statement = sns_connection
        .prepare("SELECT tid, user_name, content FROM SnsTimeLine ORDER BY tid DESC LIMIT ?1")
        .unwrap();
    let rows = statement
        .query_map([limit as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap();

    for row in rows {
        let (tid, user_name, content) = row.unwrap();
        let moment = parse_moment_xml(tid, &user_name, &content);
        if moment.created_at < since_unix {
            continue;
        }
        let display_name = names.get(&user_name).cloned().unwrap_or(user_name.clone());

        let media_json: Vec<serde_json::Value> = moment
            .media
            .iter()
            .map(|item| {
                let mut local_path: Option<String> = None;
                let is_video = item.media_type == Some(MEDIA_TYPE_VIDEO);
                let is_photo = item.media_type.map_or(true, |t| t == MEDIA_TYPE_PHOTO);
                if let (true, Some(total_size), Some(media_cache_dir)) =
                    (is_video && resolve_media, item.total_size, media_cache_dir.as_deref())
                {
                    for offset in [0_i64, -1, 1] {
                        let year_month = year_month_offset(moment.created_at, offset);
                        let index = month_video_index_cache
                            .entry(year_month.clone())
                            .or_insert_with(|| index_month_videos_by_size(account_root, &year_month));
                        if let Some(candidates) = index.get(&total_size) {
                            // Same rule as photos: an ambiguous match is no match.
                            if let [only] = candidates.as_slice() {
                                local_path = materialize_video(only, media_cache_dir)
                                    .map(|p| p.display().to_string());
                            }
                            break;
                        }
                    }
                }
                if let (true, Some(image_key), Some(total_size), Some(media_cache_dir)) =
                    (is_photo, image_key, item.total_size, media_cache_dir.as_deref())
                {
                    for offset in [0_i64, -1, 1] {
                        let year_month = year_month_offset(moment.created_at, offset);
                        let index = month_index_cache
                            .entry(year_month.clone())
                            .or_insert_with(|| {
                                index_month_by_decoded_size(account_root, &year_month, &image_key, xor_key)
                            });
                        if let Some(candidates) = index.get(&total_size) {
                            if let Some(encrypted_path) =
                                pick_unique_photo(candidates, item.declared_dims, &image_key, xor_key)
                            {
                                local_path = materialize_decoded_image(
                                    &encrypted_path,
                                    &image_key,
                                    xor_key,
                                    media_cache_dir,
                                )
                                .map(|p| p.display().to_string());
                            }
                            break;
                        }
                    }
                }
                serde_json::json!({
                    "url": item.url,
                    "kind": if is_video { "video" } else if is_photo { "photo" } else { "other" },
                    "declaredWidth": item.declared_dims.map(|d| d.0),
                    "declaredHeight": item.declared_dims.map(|d| d.1),
                    "localPath": local_path,
                })
            })
            .collect();

        println!(
            "{}",
            serde_json::json!({
                "tid": moment.tid,
                "author": display_name,
                "authorId": moment.author_username,
                "createdAtUnix": moment.created_at,
                "content": moment.content_desc,
                "mediaCount": media_json.len(),
                "media": media_json,
            })
        );
    }
}
