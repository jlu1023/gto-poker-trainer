//! The accumulating graded-decision store (D42): JSONL, gitignored alongside the matrix.
//! Dedupe key (hand_id, street, decision_index) makes re-ingesting a file idempotent.

use crate::aggregate::GradedRecord;
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::io::Write;
use std::path::Path;

pub type DedupeKey = (String, String, u32);

pub fn dedupe_key(r: &GradedRecord) -> DedupeKey {
    (r.hand_id.clone(), r.street.clone(), r.decision_index)
}

/// Load every parseable record; malformed lines are counted, not fatal (a half-written final
/// line after a crash must not brick the store).
pub fn load_store(path: &Path) -> Result<(Vec<GradedRecord>, usize)> {
    if !path.exists() {
        return Ok((Vec::new(), 0));
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading graded store {}", path.display()))?;
    let mut records = Vec::new();
    let mut bad = 0usize;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<GradedRecord>(line) {
            Ok(r) => records.push(r),
            Err(_) => bad += 1,
        }
    }
    Ok((records, bad))
}

/// Append records not already present (by dedupe key). Returns (appended, duplicates).
pub fn append_records(path: &Path, new: &[GradedRecord]) -> Result<(usize, usize)> {
    let (existing, _) = load_store(path)?;
    let mut keys: HashSet<DedupeKey> = existing.iter().map(dedupe_key).collect();

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating store dir {}", dir.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening graded store {}", path.display()))?;

    let mut appended = 0usize;
    let mut duplicates = 0usize;
    for rec in new {
        let key = dedupe_key(rec);
        if keys.contains(&key) {
            duplicates += 1;
            continue;
        }
        writeln!(file, "{}", serde_json::to_string(rec)?)?;
        keys.insert(key);
        appended += 1;
    }
    Ok((appended, duplicates))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregate::texture_bucket;

    fn rec(hand: &str, idx: u32) -> GradedRecord {
        let board = ["Ks".to_string(), "7h".to_string(), "2c".to_string()];
        GradedRecord {
            schema_version: 2,
            hand_id: hand.into(),
            street: "flop".into(),
            decision_index: idx,
            depth_bb: 100,
            pot_bb: 5.0,
            node_shape: "bb-root".into(),
            texture: texture_bucket(&board).into(),
            board,
            combo: "AhKh".into(),
            action_taken: "Check".into(),
            actions: vec!["Check".into(), "Bet(33)".into(), "Bet(75)".into()],
            p: vec![0.5, 0.4, 0.1],
            ev_mix: 100.0,
            ev_actions: vec![100.0, 100.1, 99.8],
            source_file: "t".into(),
            ingested_at: "t".into(),
        }
    }

    #[test]
    fn append_is_idempotent() {
        let dir = std::env::temp_dir().join("gto_history_store_test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("graded.jsonl");

        let batch = vec![rec("h1", 0), rec("h1", 1), rec("h2", 0)];
        let (a1, d1) = append_records(&path, &batch).unwrap();
        assert_eq!((a1, d1), (3, 0));
        // re-ingest the same file: all duplicates
        let (a2, d2) = append_records(&path, &batch).unwrap();
        assert_eq!((a2, d2), (0, 3));
        let (loaded, bad) = load_store(&path).unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(bad, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_lines_are_counted_not_fatal() {
        let dir = std::env::temp_dir().join("gto_history_store_test2");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("graded.jsonl");
        let good = serde_json::to_string(&rec("h1", 0)).unwrap();
        std::fs::write(&path, format!("{good}\n{{half-written")).unwrap();
        let (loaded, bad) = load_store(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(bad, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
