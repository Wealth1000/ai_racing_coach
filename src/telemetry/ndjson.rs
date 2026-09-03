//! Shared NDJSON line-reading machinery.
//!
//! Every sim that logs to newline-delimited JSON — which is all of them so
//! far — wants the same loop: stream line by line (memory stays flat; a
//! 14-minute capture is 51,383 lines), tolerate blank lines, fail *loudly* on
//! a first line that will not parse (the file is not this format at all), and
//! apply the same bad-line policy to the tail. The format itself — what a
//! line parses *into* — is the provider's business; this module only owns the
//! reading discipline around it.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;

use crate::core::{CoachError, Result};

/// Above this fraction of unparseable lines, stop rather than carry on.
///
/// A handful of bad lines at the tail of a capture is normal — a logger that
/// flushes on a timer means killing the sim mid-buffer can truncate the last
/// one. A high rate means something else is wrong, and analysing whatever
/// survives would produce a confident answer from a broken file.
const MAX_BAD_LINE_FRACTION: f64 = 0.01;

/// Read counters, reported by whatever owns the loop.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LineStats {
    pub lines: usize,
    pub parsed: usize,
    pub bad_lines: usize,
    pub blank_lines: usize,
}

/// A line-by-line NDJSON reader, format-agnostic.
///
/// `next` takes the provider's parse function (typically a
/// `serde_json::from_str` closure over its own frame type) and returns
/// parsed values one at a time. The reading discipline — blank lines, the
/// first-line hard error, the bad-line fraction, the empty-capture error —
/// is here, once, so every provider gets it identically.
pub struct NdjsonLines {
    path: PathBuf,
    /// `+ Send` so a source built on this can move onto the live pipeline's
    /// source thread; both `BufReader<File>` and `BufReader<GzDecoder<File>>`
    /// are `Send`, so this costs nothing at the call sites.
    reader: Box<dyn BufRead + Send>,
    stats: LineStats,
}

impl NdjsonLines {
    /// Open a capture. Gzip is detected by the `.gz` extension.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path).map_err(|source| CoachError::Io {
            path: path.display().to_string(),
            source,
        })?;

        let reader: Box<dyn BufRead + Send> = if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("gz"))
        {
            Box::new(BufReader::new(GzDecoder::new(file)))
        } else {
            Box::new(BufReader::new(file))
        };

        Ok(Self {
            path,
            reader,
            stats: LineStats::default(),
        })
    }

    pub fn stats(&self) -> LineStats {
        self.stats
    }

    fn io_err(&self, source: std::io::Error) -> CoachError {
        CoachError::Io {
            path: self.path.display().to_string(),
            source,
        }
    }

    /// Next parsed value, or `Ok(None)` at end of stream.
    ///
    /// `parse` returns the provider's frame or a serde error; the policy
    /// around it is this method's. A first line that fails to parse is a hard
    /// error carrying serde's message, which names the offending field — that
    /// is how a foreign capture is refused loudly rather than mis-parsed.
    pub fn next<T>(
        &mut self,
        mut parse: impl FnMut(&str) -> std::result::Result<T, serde_json::Error>,
    ) -> Result<Option<T>> {
        let mut line = String::new();
        loop {
            line.clear();
            let n = self
                .reader
                .read_line(&mut line)
                .map_err(|e| self.io_err(e))?;
            if n == 0 {
                // End of stream. An empty capture is an error, not an empty
                // result: it means the logger ran but the sim was not.
                if self.stats.parsed == 0 {
                    return Err(CoachError::EmptyCapture {
                        path: self.path.display().to_string(),
                    });
                }
                return Ok(None);
            }
            self.stats.lines += 1;

            let trimmed = line.trim();
            if trimmed.is_empty() {
                self.stats.blank_lines += 1;
                continue;
            }

            match parse(trimmed) {
                Ok(value) => {
                    self.stats.parsed += 1;
                    return Ok(Some(value));
                }
                Err(source) => {
                    // Failing on the very first content line means the file
                    // is not what we think it is — a schema change, or a
                    // different sim's format entirely.
                    if self.stats.parsed == 0 {
                        return Err(CoachError::Json {
                            path: self.path.display().to_string(),
                            line: self.stats.lines,
                            source,
                        });
                    }
                    self.stats.bad_lines += 1;
                    // Deliberately not printed per line: on a corrupt capture
                    // that would emit one message per frame, tens of
                    // thousands of them. The count is reported once, by the
                    // caller.
                    let seen = self.stats.parsed + self.stats.bad_lines;
                    if self.stats.bad_lines as f64 / seen as f64 > MAX_BAD_LINE_FRACTION
                        && seen > 100
                    {
                        return Err(CoachError::Json {
                            path: self.path.display().to_string(),
                            line: self.stats.lines,
                            source,
                        });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    /// A stand-in provider format: one field, so the tests exercise the
    /// *policy*, not any particular schema.
    #[derive(Debug, PartialEq, Deserialize)]
    struct Tiny {
        v: i32,
    }

    fn parse(s: &str) -> std::result::Result<Tiny, serde_json::Error> {
        serde_json::from_str(s)
    }

    fn write(path: &Path, contents: &str) {
        std::fs::write(path, contents).expect("write fixture");
    }

    #[test]
    fn blank_lines_are_tolerated_and_counted() {
        let dir = std::env::temp_dir().join("coach_ndjson_tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("blank_lines.ndjson");
        write(&path, "\n{\"v\":1}\n\n\n{\"v\":2}\n");
        let mut r = NdjsonLines::open(&path).unwrap();
        assert_eq!(r.next(parse).unwrap(), Some(Tiny { v: 1 }));
        assert_eq!(r.next(parse).unwrap(), Some(Tiny { v: 2 }));
        assert_eq!(r.next(parse).unwrap(), None);
        assert_eq!(
            r.stats(),
            LineStats {
                lines: 5,
                parsed: 2,
                bad_lines: 0,
                blank_lines: 3,
            }
        );
    }

    #[test]
    fn a_first_line_that_will_not_parse_is_a_hard_error() {
        let dir = std::env::temp_dir().join("coach_ndjson_tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("foreign.ndjson");
        write(&path, "{\"completely\": \"different\"}\n{\"v\":1}\n");
        let mut r = NdjsonLines::open(&path).unwrap();
        let err = r.next(parse).unwrap_err();
        // The error must name the file and carry serde's message, which names
        // the missing field — a foreign format is refused loudly.
        assert!(err.to_string().contains("foreign.ndjson"), "{err}");
        assert!(err.to_string().contains("missing field"), "{err}");
    }

    #[test]
    fn an_empty_capture_is_an_error_not_an_empty_result() {
        let dir = std::env::temp_dir().join("coach_ndjson_tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("empty.ndjson");
        write(&path, "");
        let mut r = NdjsonLines::open(&path).unwrap();
        assert!(matches!(
            r.next(parse),
            Err(CoachError::EmptyCapture { .. })
        ));
    }

    #[test]
    fn a_small_tail_of_bad_lines_is_survivable() {
        let dir = std::env::temp_dir().join("coach_ndjson_tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("truncated_tail.ndjson");
        // 101 good lines, then one truncated line — under the 1% fraction.
        let mut contents = String::new();
        for v in 0..101 {
            contents.push_str(&format!("{{\"v\":{v}}}\n"));
        }
        contents.push_str("{\"v\":10"); // truncated mid-object
        write(&path, &contents);
        let mut r = NdjsonLines::open(&path).unwrap();
        let mut count = 0;
        while r.next(parse).unwrap().is_some() {
            count += 1;
        }
        assert_eq!(count, 101);
        assert_eq!(r.stats().bad_lines, 1);
    }

    #[test]
    fn a_high_rate_of_bad_lines_stops_the_read() {
        let dir = std::env::temp_dir().join("coach_ndjson_tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("corrupt.ndjson");
        // Half the lines are garbage: far past the fraction, once enough have
        // been seen.
        let mut contents = String::new();
        for v in 0..200 {
            contents.push_str(&format!("{{\"v\":{v}}}\n"));
            contents.push_str("garbage\n");
        }
        write(&path, &contents);
        let mut r = NdjsonLines::open(&path).unwrap();
        // Each `next` call returns the next *good* line; the policy trips when
        // a bad line is read past the minimum sample (seen > 100), which here
        // happens inside the ~52nd call. The read must stop *before EOF*.
        let mut good = 0;
        loop {
            match r.next(parse) {
                Err(_) => break,
                Ok(Some(_)) => good += 1,
                Ok(None) => panic!("the bad-line fraction should stop the read before EOF"),
            }
        }
        assert!(good < 200, "the read should stop early, not at EOF");
    }

    #[test]
    fn gz_captures_read_directly_by_extension() {
        let dir = std::env::temp_dir().join("coach_ndjson_tests");
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("gz.ndjson");
        let gz = dir.join("gz.ndjson.gz");
        write(&raw, "{\"v\":7}\n");
        let mut encoder =
            flate2::write::GzEncoder::new(std::fs::File::create(&gz).unwrap(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, b"{\"v\":7}\n").unwrap();
        encoder.finish().unwrap();
        let mut r = NdjsonLines::open(&gz).unwrap();
        assert_eq!(r.next(parse).unwrap(), Some(Tiny { v: 7 }));
    }
}
