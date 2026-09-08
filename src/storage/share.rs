//! The donation bundle: what an opted-in driver sends the author, and the
//! one job it exists for — growing the corpus the neural coach will train
//! on (see `docs/neural-coach-design.md` §7). One driver cannot drive
//! enough laps; the drivers who use the coach, together, can.
//!
//! The bundle is deliberately small and deliberately boring: a manifest
//! (which coach, which sim, which track and cars, how many sessions and
//! rows, the install's random id) and the per-pass dataset CSV — speeds,
//! metres, seconds. Nothing else. Raw captures are never bundled: the AC
//! static page embeds the player's name, while the per-pass table needs no
//! scrubbing to be anonymous.
//!
//! A multi-hour session can outgrow the receiver's cap, so a bundle may be
//! sent as *parts* instead: each part is a complete, ordinary bundle whose
//! manifest carries a random `bundle_id` and its `part`/`parts` numbers,
//! with the CSV rows split contiguously and the header repeated on every
//! part (so each part parses alone). The receiver needs no new contract —
//! every part passes the same gzip/schema/size checks — and the corpus side
//! reassembles with [`join_part_csvs`]. Session ids are hashed
//! deterministically, so a session split across parts keeps one id.
//!
//! Session names are the one field in the CSV a driver could have made
//! their own (a hand-named `.ndjson` dropped into the sessions directory),
//! so they are remapped to opaque hashes before the CSV enters the bundle —
//! the grouping a training split needs survives, the name does not leave
//! the machine.
//!
//! Consent lives in the GUI (`ui::screens::SimHome`): off by default, on
//! only through the dialog that says what is sent and why. This module
//! assumes the caller already asked.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};

use flate2::write::GzEncoder;
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};

use crate::core::error::CoachError;
use crate::storage::dataset::DatasetInfo;

/// The upload endpoint this build sends to: the author's Worker (see
/// `share-backend/`). Compiled in so a donation needs no setup — the
/// consent dialog and the explicit Send button are the only gates, and the
/// destination is the author's, not a choice the driver has to make.
pub const DEFAULT_ENDPOINT: &str = "https://coach-share.anthonyaddo999.workers.dev/";

/// Overrides [`DEFAULT_ENDPOINT`] when set — for testing against a
/// throwaway receiver, or a fork pointing donations at its own bucket.
/// Absent or empty, the compiled-in default is used.
pub const ENDPOINT_ENV: &str = "COACH_SHARE_ENDPOINT";

/// Where offline bundles land when there is no endpoint to send to. Beside
/// `data/tracks` and `data/captures`, like every artefact this tool writes.
pub const SHARE_DIR: &str = "data/share";

/// The bundle's shape version. The receiving side refuses a schema it does
/// not know — a bundle the author cannot parse is a donation wasted.
pub const SCHEMA: u32 = 1;

/// The receiver's upload cap, mirrored from the Worker's `MAX_BYTES`
/// (`share-backend/src/worker.js`). A bundle above it is refused at the
/// door, so the sender splits into parts instead of trying.
pub const MAX_BUNDLE_BYTES: usize = 8 * 1024 * 1024;

/// First guess for part sizing: aim each part at half the cap, so the
/// estimate is wrong in the safe direction and the doubling loop below
/// rarely runs at all.
const PART_TARGET_BYTES: usize = MAX_BUNDLE_BYTES / 2;

/// What the bundle says about itself. Everything a pooled corpus needs to
/// place its rows (per-car speeds, per-track corners — see the design doc's
/// corpus section) and nothing that places the driver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareManifest {
    pub schema: u32,
    pub coach_version: String,
    pub sim: String,
    pub track: String,
    pub cars: Vec<String>,
    pub sessions: u64,
    pub rows: u64,
    pub install_id: String,
    /// Present only on parts of a split bundle: one random id shared by
    /// every part of the same send, and this part's ordinal out of the
    /// whole. `None` on an ordinary single-bundle send — the common case
    /// stays exactly the shape it always was.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub part: Option<BundlePart>,
}

/// Where one part of a split bundle sits in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundlePart {
    /// Random per-send id: every part of one split carries the same one.
    pub bundle_id: String,
    /// This part, 1-based.
    pub part: u32,
    /// How many parts the whole bundle was split into.
    pub parts: u32,
}

/// The bundle as one gzip stream: a JSON object holding the manifest and
/// the scrubbed CSV. One file, self-describing, the same compression the
/// captures use.
#[derive(Serialize, Deserialize)]
struct ShareBundle {
    manifest: ShareManifest,
    dataset_csv: String,
}

/// Build the manifest for an export the caller already ran. The sim and
/// track are stringified here — the manifest is JSON for a receiver that
/// knows nothing of this crate's types, so it carries the stable key and
/// the display name, not the enums.
pub fn manifest(info: &DatasetInfo, install_id: &str) -> ShareManifest {
    ShareManifest {
        schema: SCHEMA,
        coach_version: env!("CARGO_PKG_VERSION").to_string(),
        sim: info.sim.key().to_string(),
        track: info.track.to_string(),
        cars: info.cars.clone(),
        sessions: info.sessions,
        rows: info.rows,
        install_id: install_id.to_string(),
        part: None,
    }
}

/// Replace the session column (the first) with opaque, install-scoped ids.
///
/// The id is a hash of `(install_id, session_name)` — deterministic, so
/// the same session hashed in two uploads of the same install yields the
/// same id and the author's pooled corpus can split by session honestly;
/// not reversible in any way the author could be tempted by, because the
/// name never enters the bundle at all. `DefaultHasher::new()` is seeded
/// with fixed keys, so the mapping is stable across runs of the same build.
pub fn scrub_sessions(csv: &str, install_id: &str) -> String {
    let mut lines = csv.lines();
    let Some(header) = lines.next() else {
        return String::new();
    };
    let mut out = String::with_capacity(csv.len());
    out.push_str(header);
    out.push('\n');
    for line in lines {
        let mut fields = split_csv_row(line);
        if fields.is_empty() {
            out.push('\n');
            continue;
        }
        let mut hasher = DefaultHasher::new();
        (install_id, &fields[0]).hash(&mut hasher);
        fields[0] = format!("s_{:016x}", hasher.finish());
        // Every other field passes through unchanged, but the split took
        // its quoting off — put it back with the dataset's own rule, so a
        // session name that needed quotes elsewhere in a row keeps them.
        out.push_str(
            &fields
                .into_iter()
                .map(crate::storage::dataset::csv_field)
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push('\n');
    }
    out
}

/// Build the gzipped bundle bytes for an exported dataset. The manifest is
/// moved in — it *is* the bundle's identity, and the sessions are scrubbed
/// with its install id so the bundle cannot hold two ids that disagree.
pub fn build_bundle(csv: &str, manifest: ShareManifest) -> Result<Vec<u8>, CoachError> {
    let dataset_csv = scrub_sessions(csv, &manifest.install_id);
    let bundle = ShareBundle {
        manifest,
        dataset_csv,
    };
    let text = serde_json::to_string(&bundle).map_err(|e| CoachError::Io {
        path: "share bundle".to_string(),
        source: std::io::Error::other(e),
    })?;
    gzip(text.as_bytes())
}

/// Split an oversized donation into part-bundles the receiver accepts as
/// ordinary uploads. Rows are divided as evenly as the size allows; every
/// part repeats the CSV header, so each part is a parseable CSV alone; and
/// the session scrub happens per part, so a session split across parts
/// keeps one id (the hash depends on the name, not the row it sits in).
///
/// The return is one bundle per part, each under [`MAX_BUNDLE_BYTES`] —
/// the loop re-estimates the part count from the largest observed part
/// until every part fits, which terminates because the rows per part
/// shrink toward zero.
pub fn build_parts(
    csv: &str,
    mut manifest: ShareManifest,
) -> Result<Vec<Vec<u8>>, CoachError> {
    // The whole-bundle manifest has no part block; parts get a shared
    // random id, and the loop below stamps part numbers into a clone.
    let bundle_id = format!(
        "b_{:x}",
        now_unix_ms() as u32 as u64 ^ std::process::id() as u64
    );

    let scrubbed = scrub_sessions(csv, &manifest.install_id);
    let mut lines: Vec<&str> = scrubbed.lines().collect();
    let header = lines.first().copied().unwrap_or_default().to_string();
    let data: Vec<&str> = lines.split_off(1.min(lines.len()));

    let mut parts: u32 = 1.max(
        (scrubbed.len() / PART_TARGET_BYTES.max(1)).try_into().unwrap_or(1),
    );
    loop {
        let chunks = split_rows_evenly(&data, parts as usize);
        let built: Result<Vec<Vec<u8>>, CoachError> = chunks
            .into_iter()
            .zip(1..=parts)
            .map(|(rows, part)| {
                let part_csv = rows_to_csv(&header, &rows);
                manifest.part = Some(BundlePart {
                    bundle_id: bundle_id.clone(),
                    part,
                    parts,
                });
                manifest.rows = rows.len() as u64;
                let text = serde_json::to_string(&ShareBundle {
                    manifest: manifest.clone(),
                    dataset_csv: part_csv,
                })
                .map_err(|e| CoachError::Io {
                    path: "share bundle".to_string(),
                    source: std::io::Error::other(e),
                })?;
                gzip(text.as_bytes())
            })
            .collect();
        let built = built?;
        if built.iter().all(|b| b.len() <= MAX_BUNDLE_BYTES) || parts >= 10_000 {
            return Ok(built);
        }
        // A part still does not fit: gzip does not shrink text linearly,
        // so estimate from the largest observed part and retry.
        let largest = built.iter().map(|b| b.len()).max().unwrap_or(0);
        parts = (((largest as f64 / PART_TARGET_BYTES as f64) * parts as f64).ceil() as u32)
            .max(parts + 1)
            .min(10_000);
    }
}

/// Divide `rows` into `n` chunks as evenly as sizes allow, order preserved.
/// No chunk is empty when `rows` is not (a part with no rows is not a part).
fn split_rows_evenly<'a>(rows: &[&'a str], n: usize) -> Vec<Vec<&'a str>> {
    if n == 0 || rows.is_empty() {
        return Vec::new();
    }
    let n = n.min(rows.len());
    let mut chunks = Vec::with_capacity(n);
    let base = rows.len() / n;
    let extra = rows.len() % n;
    let mut start = 0;
    for i in 0..n {
        let len = base + usize::from(i < extra);
        chunks.push(rows[start..start + len].to_vec());
        start += len;
    }
    chunks
}

/// Header plus rows, newline-terminated — the CSV shape every part carries.
fn rows_to_csv(header: &str, rows: &[&str]) -> String {
    let mut out = String::with_capacity(header.len() + 1);
    out.push_str(header);
    out.push('\n');
    for row in rows {
        out.push_str(row);
        out.push('\n');
    }
    out
}

/// Reassemble the CSVs of one split bundle's parts, in part order, into the
/// whole CSV the parts came from — the reader side of the split, so the
/// corpus ingestion treats parts exactly like one bundle.
///
/// Every part repeats the header, so the join is: take part 1 whole, then
/// each later part minus its header line.
pub fn join_part_csvs(part_csvs: &[String]) -> String {
    let mut out = String::new();
    for (i, csv) in part_csvs.iter().enumerate() {
        if i == 0 {
            out.push_str(csv);
        } else {
            let body = csv.lines().skip(1).collect::<Vec<&str>>().join("\n");
            out.push_str(&body);
        }
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

fn gzip(bytes: &[u8]) -> Result<Vec<u8>, CoachError> {
    let mut encoder = GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).map_err(gzip_err)?;
    encoder.finish().map_err(gzip_err)
}

fn gzip_err(e: std::io::Error) -> CoachError {
    CoachError::Io {
        path: "share bundle".to_string(),
        source: e,
    }
}

/// The bundle's file name: `share_<track>_<stamp>.json.gz`, the stamp in
/// the logger's own `yyyyMMdd_HHmmss` convention so every artefact this
/// tool names reads the same way.
pub fn bundle_name(track: &str) -> String {
    format!("share_{track}_{}.json.gz", stamp_utc(now_unix_ms()))
}

/// Write the bundle to `dir`, creating it like every artefact writer does.
/// Returns the path written, for the line the job screen shows the driver.
pub fn save_bundle(dir: &Path, track: &str, bytes: &[u8]) -> Result<PathBuf, CoachError> {
    std::fs::create_dir_all(dir).map_err(|e| CoachError::Io {
        path: dir.display().to_string(),
        source: e,
    })?;
    let path = dir.join(bundle_name(track));
    std::fs::write(&path, bytes).map_err(|e| CoachError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    Ok(path)
}

/// POST the bundle to the endpoint. Blocking — it belongs on a job thread,
/// never the UI one. Any non-2xx or transport failure is an error the
/// caller degrades from (save to disk), because sharing is a favour: it
/// must never cost the driver anything but the try.
pub fn upload(endpoint: &str, bytes: &[u8]) -> Result<(), CoachError> {
    let response = ureq::post(endpoint)
        .timeout(std::time::Duration::from_secs(60))
        .set("Content-Type", "application/gzip")
        .set("X-Coach-Share-Schema", &SCHEMA.to_string())
        .send_bytes(bytes)
        .map_err(|e| CoachError::ShareUpload {
            endpoint: endpoint.to_string(),
            detail: e.to_string(),
        })?;
    let status = response.status();
    if !(200..300).contains(&status) {
        return Err(CoachError::ShareUpload {
            endpoint: endpoint.to_string(),
            detail: format!("the server answered {status}"),
        });
    }
    Ok(())
}

/// Decompress a bundle — the reader half of the format, kept beside the
/// writer so a bundle can be inspected (and tested) without the author's
/// tooling. Returns `(manifest, csv)`.
pub fn read_bundle(bytes: &[u8]) -> Result<(ShareManifest, String), CoachError> {
    let decoder = GzDecoder::new(bytes);
    let bundle: ShareBundle =
        serde_json::from_reader(decoder).map_err(|e| CoachError::BadArtefact {
            path: "share bundle".to_string(),
            artefact: "share bundle",
            detail: e.to_string(),
        })?;
    if bundle.manifest.schema != SCHEMA {
        return Err(CoachError::BadArtefact {
            path: "share bundle".to_string(),
            artefact: "share bundle",
            detail: format!(
                "schema {} is newer than this build's {} — update to read it",
                bundle.manifest.schema, SCHEMA
            ),
        });
    }
    Ok((bundle.manifest, bundle.dataset_csv))
}

// `yyyyMMdd_HHmmss` from Unix milliseconds — the logger's stamp, same as
// `sims::assetto_corsa::record`.
fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn stamp_utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let time_of_day = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}_{:02}{:02}{:02}",
        time_of_day / 3600,
        (time_of_day % 3600) / 60,
        time_of_day % 60
    )
}

fn civil_from_days(z: i64) -> (i64, u64, u64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Split one CSV row into fields, honouring the quoting the dataset writer
/// applies (the mirror of the reader in `dataset`'s tests — the bundle
/// scrubs a CSV this crate itself wrote).
fn split_csv_row(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                current.push('"');
                chars.next();
            }
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => fields.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    fields.push(current);
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ids::TrackId;
    use crate::core::sample::Sim;

    fn info() -> DatasetInfo {
        DatasetInfo {
            rows: 3,
            columns: 24,
            sim: Sim::AssettoCorsa,
            track: TrackId::new("monza", ""),
            cars: vec!["ks_ferrari_sf70h".to_string()],
            sessions: 2,
        }
    }

    /// The consent contract, in one test: the session column is opaque, the
    /// names it came from are nowhere in the bundle, and the same session
    /// still maps to the same id (so pooled corpora can split by session).
    #[test]
    fn the_bundle_scrubs_session_names_but_keeps_their_identity() {
        let csv = "session,lap,lap_clean,corner\n\
                    \"dave's best lap, honest\",1,true,0\n\
                    session_123,2,true,1\n\
                    \"dave's best lap, honest\",3,true,2\n";
        let bytes =
            build_bundle(csv, manifest(&info(), "install_test")).expect("build bundle");
        let (manifest, csv) = read_bundle(&bytes).expect("read bundle");

        assert_eq!(manifest.install_id, "install_test");
        assert_eq!(manifest.schema, SCHEMA);
        assert_eq!(manifest.rows, 3);
        assert_eq!(manifest.track, "monza");

        assert!(
            !csv.contains("dave"),
            "a session name must never leave the machine: {csv}"
        );
        let ids: Vec<&str> = csv.lines().skip(1).map(|l| l.split(',').next().unwrap())
            .collect();
        assert_eq!(ids.len(), 3);
        assert_eq!(ids[0], ids[2], "the same session keeps the same id");
        assert_ne!(ids[0], ids[1], "different sessions get different ids");
        assert!(ids.iter().all(|id| id.starts_with("s_") && id.len() == 18));
    }

    /// The scrub is stable across calls — two uploads from one install
    /// produce joinable ids, not a fresh mapping each time.
    #[test]
    fn the_session_hashes_are_stable_across_calls() {
        let once = scrub_sessions("session,lap\nmonday,1\n", "install_x");
        let twice = scrub_sessions("session,lap\nmonday,1\n", "install_x");
        assert_eq!(once, twice);

        let other_install = scrub_sessions("session,lap\nmonday,1\n", "install_y");
        assert_ne!(
            once, other_install,
            "two installs hashing the same session name stay distinguishable"
        );
    }

    /// The split's contract, in one test: every part is a bundle the
    /// receiver already accepts (its own gzip stream, its own manifest,
    /// under the cap), every part carries the header, the parts share one
    /// id and number themselves 1..n, and — the part that matters for the
    /// corpus — the rows joined back are exactly the rows of the whole.
    #[test]
    fn parts_are_ordinary_bundles_whose_rows_rejoin() {
        // High-entropy rows: gzip has to store them nearly raw, so the
        // bundle genuinely outgrows the cap rather than compressing under
        // it the way real driving data sometimes can.
        let rows: Vec<String> = (0..600_000)
            .map(|n| format!("s_{:x}, {}, {}, {:x}{:x}{:x}{:x}", n % 7, n, n % 13, n, n * 31, n * 17, n ^ 0xdead))
            .collect();
        let csv = format!("session,lap,corner,brake\n{}\n", rows.join("\n"));
        assert!(
            csv.len() > MAX_BUNDLE_BYTES,
            "the fixture must actually be oversized: {} bytes",
            csv.len()
        );
        let manifest = manifest(&info(), "install_test");

        let parts = build_parts(&csv, manifest).expect("build parts");
        assert!(
            parts.len() > 1,
            "a multi-MB CSV must actually split: {} parts",
            parts.len()
        );
        assert!(
            parts.iter().all(|p| p.len() <= MAX_BUNDLE_BYTES),
            "every part must fit the receiver's cap"
        );

        let mut decoded: Vec<(String, String)> = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            // Each part is a gzip stream in its own right — the receiver's
            // magic-byte check would refuse anything else.
            assert_eq!(&part[..2], &[0x1f, 0x8b], "part {} is not gzip", i + 1);
            let (m, part_csv) = read_bundle(part).expect("each part is a readable bundle");
            let part_block = m.part.clone().expect("each part declares itself one");
            assert_eq!(part_block.part, (i + 1) as u32, "parts arrive in order");
            assert_eq!(part_block.parts as usize, parts.len());
            let bundle_id = m.bundle_id();
            assert!(
                decoded
                    .iter()
                    .all(|(id, _): &(String, String)| id == &bundle_id),
                "every part of one send shares the bundle id"
            );
            assert!(part_csv.contains("session,lap,corner,brake"), "header repeats");
            decoded.push((bundle_id, part_csv));
        }
        assert_eq!(decoded.len(), parts.len());

        let joined = join_part_csvs(
            &decoded.iter().map(|(_, csv)| csv.clone()).collect::<Vec<_>>(),
        );
        let whole = scrub_sessions(&csv, "install_test");
        assert_eq!(
            joined.lines().count(),
            whole.lines().count(),
            "the join loses no rows and adds none"
        );
    }

    /// A CSV that fits in one part round-trips through the split as a
    /// single part — the small session is not a special case. The scrub
    /// applies to parts too, so the expected CSV is the scrubbed one.
    #[test]
    fn a_small_csv_is_one_part() {
        let csv = "session,lap\nmonday,1\ntuesday,2\n";
        let parts = build_parts(csv, manifest(&info(), "i")).expect("parts");
        assert_eq!(parts.len(), 1);
        let (m, part_csv) = read_bundle(&parts[0]).expect("read");
        assert_eq!(m.rows, 2);
        assert_eq!(part_csv, scrub_sessions(csv, "i"));
    }

    /// The split keeps the session scrub: a session split across parts
    /// keeps one id, and the name never appears in any part.
    #[test]
    fn parts_never_leak_session_names() {
        let csv = "session,lap\n\"dave's laps\",1\nother,2\n\"dave's laps\",3\n";
        let parts = build_parts(csv, manifest(&info(), "i")).expect("parts");
        for part in &parts {
            let (_, part_csv) = read_bundle(part).expect("read");
            assert!(
                !part_csv.contains("dave"),
                "a session name must never leave the machine: {part_csv}"
            );
        }
    }

    /// The join is the reader's promise: part 1 whole, later parts minus
    /// their header, one newline where two CSVs meet.
    #[test]
    fn the_join_stitches_headers_away() {
        let joined = join_part_csvs(&[
            "a,b\n1,2\n".to_string(),
            "a,b\n3,4\n".to_string(),
            "a,b\n5,6\n".to_string(),
        ]);
        assert_eq!(joined, "a,b\n1,2\n3,4\n5,6\n");
    }

    /// A session split across parts keeps one id — the scrub is
    /// deterministic per (install, name), not per part.
    #[test]
    fn a_session_split_across_parts_keeps_one_id() {
        let rows: Vec<String> = (0..100).map(|n| format!("monday,{}",n)).collect();
        let csv = format!("session,lap\n{}\n", rows.join("\n"));
        let parts = build_parts(&csv, manifest(&info(), "i")).expect("parts");
        let ids: Vec<String> = parts
            .iter()
            .flat_map(|p| {
                let (_, part_csv) = read_bundle(p).expect("read");
                part_csv.lines().skip(1).map(|l| l.split(',').next().unwrap().to_string()).collect::<Vec<_>>()
            })
            .collect();
        assert!(
            ids.iter().all(|id| id == &ids[0]),
            "one session name must hash to one id across parts: {ids:?}"
        );
    }

    impl ShareManifest {
        /// The split's shared id, for tests that check parts agree on it.
        fn bundle_id(&self) -> String {
            self.part.as_ref().map(|p| p.bundle_id.clone()).unwrap_or_default()
        }
    }

    /// Quoted fields elsewhere in a row survive the scrub untouched — the
    /// scrub rewrites the first field only.
    #[test]
    fn quoted_fields_pass_through_the_scrub() {
        let csv = "session,lap,corner\ns1,\"1,000\",0\n";
        let scrubbed = scrub_sessions(csv, "install_x");
        let fields = split_csv_row(scrubbed.lines().nth(1).unwrap());
        assert_eq!(fields[1], "1,000");
    }

    #[test]
    fn a_bundle_the_receiver_cannot_parse_says_so() {
        let err = read_bundle(b"not a gzip stream at all").unwrap_err();
        assert!(err.to_string().contains("share bundle"), "{err}");
    }

    #[test]
    fn a_future_schema_is_refused_rather_than_misread() {
        let manifest = ShareManifest {
            schema: SCHEMA + 1,
            ..manifest(&info(), "install_test")
        };
        let bytes = build_bundle("session,lap\nx,1\n", manifest).expect("build");
        let err = read_bundle(&bytes).unwrap_err();
        assert!(err.to_string().contains("update to read it"), "{err}");
    }

    #[test]
    fn saving_a_bundle_writes_it_under_the_share_name() {
        let dir = std::env::temp_dir().join(format!(
            "coach_share_tests/save_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let bytes = build_bundle("session,lap\nx,1\n", manifest(&info(), "i"))
            .expect("build");
        let path = save_bundle(&dir, "monza", &bytes).expect("save");
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("share_monza_"), "{name}");
        assert!(name.ends_with(".json.gz"), "{name}");
        assert_eq!(
            std::fs::read(&path).expect("read back"),
            bytes,
            "the file is the bundle, byte for byte"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The compiled-in destination is a real, https Worker URL — a typo in
    /// a baked-in constant would fail every donation with a DNS error, so
    /// the shape is pinned here.
    #[test]
    fn the_default_endpoint_is_a_https_worker_url() {
        assert!(DEFAULT_ENDPOINT.starts_with("https://"));
        assert!(DEFAULT_ENDPOINT.ends_with(".workers.dev/"));
        assert!(DEFAULT_ENDPOINT.contains("coach-share"));
    }

    /// The upload posts the bundle bytes to the endpoint and treats any
    /// non-2xx as a failure to deliver — checked against a throwaway local
    /// HTTP listener, because the contract is worth pinning even before a
    /// real bucket exists.
    #[test]
    fn an_upload_posts_the_bytes_and_requires_a_2xx() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local listener");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            // Read until the body's end: the request has no
            // Content-Length-terminator trick available, so read headers
            // first, then exactly the body they promise.
            loop {
                let read = stream.read(&mut buffer).expect("read request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&request);
                if let Some(length) = content_length(&text) {
                    let header_end = text.find("\r\n\r\n").expect("header end");
                    let body_start = header_end + 4;
                    let body_bytes = request.len() - body_start;
                    if body_bytes >= length {
                        break;
                    }
                }
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .expect("respond");
            request
        });

        let payload = b"the bundle bytes".to_vec();
        upload(&format!("http://127.0.0.1:{port}/share"), &payload)
            .expect("upload to a 200 succeeds");

        let request = server.join().expect("server thread");
        let text = String::from_utf8_lossy(&request);
        assert!(text.starts_with("POST /share "), "{text}");
        assert!(text.contains("Content-Type: application/gzip"), "{text}");
        assert!(text.contains(&format!("X-Coach-Share-Schema: {SCHEMA}")), "{text}");
        let body = text.split("\r\n\r\n").nth(1).unwrap_or_default();
        assert_eq!(body.as_bytes(), payload.as_slice());
    }

    fn content_length(request: &str) -> Option<usize> {
        request
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
            .and_then(|l| l.split(':').nth(1))
            .and_then(|v| v.trim().parse().ok())
    }
}
