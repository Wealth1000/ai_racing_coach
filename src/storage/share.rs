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
