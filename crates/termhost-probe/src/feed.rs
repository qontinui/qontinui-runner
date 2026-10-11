//! Where the engine learns which markers the workload emitted, and when:
//! the AI-session generators' `<run-dir>/gen-<k>.jsonl` (Phase 1's record
//! format: `start`, `marker`, `echo`, `progress`, `exit`), tailed while they
//! are written, or the selftest's own schedule.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::codec::MarkerKind;
use crate::model::{EmitSource, Emitted};

/// A source of emitted markers.
pub trait EmitFeed: Send {
    /// Markers emitted since the last poll.
    fn poll(&mut self) -> Vec<Emitted>;
    fn describe(&self) -> String;
}

/// Parse one `gen-<k>.jsonl` line. `session` is the file's session (set by
/// its `start` record; the slot number until then).
pub fn parse_gen_line(line: &str, session: &mut Option<u16>) -> Option<Emitted> {
    let v: Value = serde_json::from_str(line).ok()?;
    match v.get("type")?.as_str()? {
        "start" => {
            if let Some(s) = v
                .get("session")
                .and_then(Value::as_u64)
                .and_then(|s| u16::try_from(s).ok())
            {
                *session = Some(s);
            }
            None
        }
        "marker" => {
            let kind = match v.get("kind")?.as_str()? {
                "output" => MarkerKind::Output,
                "calibration" => MarkerKind::Calibration,
                _ => return None,
            };
            emitted(&v, kind, (*session)?)
        }
        "echo" => emitted(&v, MarkerKind::Echo, (*session)?),
        _ => None,
    }
}

fn emitted(v: &Value, kind: MarkerKind, session: u16) -> Option<Emitted> {
    let seq_full = v.get("seq")?.as_u64()?;
    // Absolute hrtime values are decimal strings (they exceed 2^53).
    let t_emit_ns = match v.get("t_emit_ns")? {
        Value::String(s) => s.parse().ok()?,
        Value::Number(n) => n.as_u64()?,
        _ => return None,
    };
    Some(Emitted {
        session,
        kind,
        seq_full,
        t_emit_ns,
        source: EmitSource::Generator,
    })
}

/// The slot number in `gen-<k>.jsonl`.
fn slot_of(path: &Path) -> Option<u16> {
    let name = path.file_name()?.to_str()?;
    name.strip_prefix("gen-")?
        .strip_suffix(".jsonl")?
        .parse()
        .ok()
}

#[derive(Debug, Default)]
struct Tail {
    offset: u64,
    partial: Vec<u8>,
    session: Option<u16>,
}

/// Tails every `gen-*.jsonl` in a run directory, picking up files that
/// appear later.
pub struct GenFeed {
    dir: PathBuf,
    tails: BTreeMap<PathBuf, Tail>,
    malformed: u64,
}

impl GenFeed {
    pub fn new(dir: &Path) -> GenFeed {
        GenFeed {
            dir: dir.to_path_buf(),
            tails: BTreeMap::new(),
            malformed: 0,
        }
    }

    /// Lines that were not parseable records (a partial line is not one).
    pub fn malformed(&self) -> u64 {
        self.malformed
    }

    fn discover(&mut self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if slot_of(&path).is_some() && !self.tails.contains_key(&path) {
                let session = slot_of(&path);
                self.tails.insert(
                    path,
                    Tail {
                        session,
                        ..Tail::default()
                    },
                );
            }
        }
    }
}

impl EmitFeed for GenFeed {
    fn poll(&mut self) -> Vec<Emitted> {
        self.discover();
        let mut out = Vec::new();
        for (path, tail) in self.tails.iter_mut() {
            let Ok(mut f) = File::open(path) else {
                continue;
            };
            if f.seek(SeekFrom::Start(tail.offset)).is_err() {
                continue;
            }
            let mut buf = Vec::new();
            let Ok(n) = f.read_to_end(&mut buf) else {
                continue;
            };
            tail.offset += n as u64;
            tail.partial.extend_from_slice(&buf);
            // Only complete lines; a line being written stays partial.
            while let Some(nl) = tail.partial.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = tail.partial.drain(..=nl).collect();
                let Ok(text) = std::str::from_utf8(&line) else {
                    self.malformed += 1;
                    continue;
                };
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                match parse_gen_line(text, &mut tail.session) {
                    Some(e) => out.push(e),
                    None => {
                        if serde_json::from_str::<Value>(text).is_err() {
                            self.malformed += 1;
                        }
                    }
                }
            }
        }
        out
    }

    fn describe(&self) -> String {
        format!(
            "{}/gen-*.jsonl ({} files)",
            self.dir.display(),
            self.tails.len()
        )
    }
}

/// A feed the selftest pushes its own schedule and injections into.
#[derive(Clone, Default)]
pub struct SharedFeed(Arc<Mutex<Vec<Emitted>>>);

impl SharedFeed {
    pub fn new() -> SharedFeed {
        SharedFeed::default()
    }

    pub fn push(&self, e: Emitted) {
        self.0.lock().expect("shared feed").push(e);
    }
}

impl EmitFeed for SharedFeed {
    fn poll(&mut self) -> Vec<Emitted> {
        std::mem::take(&mut *self.0.lock().expect("shared feed"))
    }

    fn describe(&self) -> String {
        "the selftest's own schedule".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_the_phase_1_record_shapes() {
        let mut session = Some(9);
        assert!(
            parse_gen_line(r#"{"type":"start","session":3,"t0_ns":"5"}"#, &mut session).is_none()
        );
        assert_eq!(session, Some(3));
        let m = parse_gen_line(
            r#"{"type":"marker","kind":"output","seq":120000,"phase":"idle","t_sched_ns":"1","t_emit_ns":"18446744073709551000","t_written_ns":"2","bytes":9}"#,
            &mut session,
        )
        .expect("marker");
        assert_eq!(
            (m.session, m.kind, m.seq_full, m.t_emit_ns),
            (3, MarkerKind::Output, 120_000, 18_446_744_073_709_551_000)
        );
        let e = parse_gen_line(
            r#"{"type":"echo","seq":4,"byte":97,"t_read_ns":"1","t_emit_ns":"77","t_written_ns":"78","bytes":10}"#,
            &mut session,
        )
        .expect("echo");
        assert_eq!((e.kind, e.seq_full, e.t_emit_ns), (MarkerKind::Echo, 4, 77));
        assert!(parse_gen_line(r#"{"type":"progress","t_ns":"1"}"#, &mut session).is_none());
    }

    #[test]
    fn tails_files_line_by_line_including_late_ones() {
        let dir = std::env::temp_dir().join(format!("termhost-probe-feed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let mut feed = GenFeed::new(&dir);
        assert!(feed.poll().is_empty());

        let p0 = dir.join("gen-0.jsonl");
        let mut f0 = File::create(&p0).expect("create");
        writeln!(f0, r#"{{"type":"start","session":0}}"#).expect("write");
        write!(
            f0,
            r#"{{"type":"marker","kind":"output","seq":1,"t_emit_ns":"100"}}"#
        )
        .expect("write");
        f0.flush().expect("flush");
        assert!(feed.poll().is_empty(), "a partial line is not consumed");
        writeln!(f0).expect("write");
        writeln!(
            f0,
            r#"{{"type":"marker","kind":"output","seq":2,"t_emit_ns":"200"}}"#
        )
        .expect("write");
        f0.flush().expect("flush");
        let got = feed.poll();
        assert_eq!(
            got.iter().map(|e| e.seq_full).collect::<Vec<_>>(),
            vec![1, 2]
        );

        let mut f1 = File::create(dir.join("gen-1.jsonl")).expect("create");
        writeln!(f1, r#"{{"type":"start","session":1}}"#).expect("write");
        writeln!(
            f1,
            r#"{{"type":"marker","kind":"output","seq":0,"t_emit_ns":"300"}}"#
        )
        .expect("write");
        f1.flush().expect("flush");
        let got = feed.poll();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].session, 1);
        assert_eq!(feed.malformed(), 0);
        drop((f0, f1));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
