//! Compressed history chunk store with paged reads (SUM-52 / §8.2).
//!
//! Lines evicted from the resident ring buffer are appended here and persisted as
//! zstd-compressed JSON-Lines chunk files (not database blobs). History is read back in pages
//! so a UI can scroll arbitrarily far without loading everything into memory.

use crate::line::StoredLine;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Metadata for one persisted chunk file.
#[derive(Clone, Debug)]
pub struct ChunkMeta {
    /// Sequence number (also the ordering key).
    pub seq: u64,
    /// Path to the compressed chunk file.
    pub path: PathBuf,
    /// Number of lines in the chunk.
    pub line_count: u64,
}

/// Appends evicted history to compressed chunk files and serves paged reads.
#[derive(Debug)]
pub struct ChunkStore {
    dir: PathBuf,
    chunk_lines: usize,
    pending: Vec<StoredLine>,
    chunks: Vec<ChunkMeta>,
    next_seq: u64,
    total_lines: u64,
    compression_level: i32,
}

impl ChunkStore {
    /// Create a chunk store rooted at `dir`, flushing a chunk every `chunk_lines` lines.
    pub fn new(dir: impl Into<PathBuf>, chunk_lines: usize) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            chunk_lines: chunk_lines.max(1),
            pending: Vec::new(),
            chunks: Vec::new(),
            next_seq: 0,
            total_lines: 0,
            compression_level: 3,
        })
    }

    /// Append evicted lines, flushing full chunks to disk as they fill.
    pub fn append(&mut self, lines: impl IntoIterator<Item = StoredLine>) -> io::Result<()> {
        for line in lines {
            self.pending.push(line);
            self.total_lines += 1;
            if self.pending.len() >= self.chunk_lines {
                self.flush_chunk()?;
            }
        }
        Ok(())
    }

    /// Flush any buffered lines to a final (possibly partial) chunk.
    pub fn flush(&mut self) -> io::Result<()> {
        self.flush_chunk()
    }

    fn flush_chunk(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        let path = self.dir.join(format!("chunk-{seq:08}.jsonl.zst"));

        let mut jsonl = Vec::new();
        for line in &self.pending {
            serde_json::to_writer(&mut jsonl, line)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            jsonl.push(b'\n');
        }
        let compressed = zstd::stream::encode_all(&jsonl[..], self.compression_level)?;
        fs::write(&path, compressed)?;

        self.chunks.push(ChunkMeta {
            seq,
            path,
            line_count: self.pending.len() as u64,
        });
        self.pending.clear();
        Ok(())
    }

    /// Total lines ever appended (flushed + pending).
    pub fn total_lines(&self) -> u64 {
        self.total_lines
    }

    /// Number of chunk files flushed to disk.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Read up to `n` lines of history starting at global line index `cursor`.
    ///
    /// Only the chunks overlapping the requested range are decompressed, so memory stays flat
    /// regardless of how far back `cursor` points.
    pub fn read_history(&self, cursor: u64, n: usize) -> io::Result<Vec<StoredLine>> {
        let mut out = Vec::new();
        let mut base: u64 = 0;
        for meta in &self.chunks {
            let chunk_end = base + meta.line_count;
            if out.len() >= n {
                break;
            }
            if chunk_end > cursor {
                let lines = read_chunk(&meta.path)?;
                for (i, line) in lines.into_iter().enumerate() {
                    let global = base + i as u64;
                    if global >= cursor && out.len() < n {
                        out.push(line);
                    }
                }
            }
            base = chunk_end;
        }
        // Serve the not-yet-flushed tail from memory.
        for (i, line) in self.pending.iter().enumerate() {
            let global = base + i as u64;
            if global >= cursor && out.len() < n {
                out.push(line.clone());
            }
        }
        Ok(out)
    }
}

fn read_chunk(path: &Path) -> io::Result<Vec<StoredLine>> {
    let compressed = fs::read(path)?;
    let jsonl = zstd::stream::decode_all(&compressed[..])?;
    let mut lines = Vec::new();
    for raw in jsonl.split(|&b| b == b'\n') {
        if raw.is_empty() {
            continue;
        }
        let line: StoredLine = serde_json::from_slice(raw)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        lines.push(line);
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("memmux-chunk-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn text(s: &str) -> StoredLine {
        StoredLine::Text {
            text: s.to_string(),
        }
    }

    #[test]
    fn append_flush_and_paged_read() {
        let dir = tmp("paged");
        let mut store = ChunkStore::new(&dir, 100).unwrap();
        for i in 0..1000 {
            store.append([text(&format!("line-{i}"))]).unwrap();
        }
        store.flush().unwrap();
        assert_eq!(store.total_lines(), 1000);
        assert_eq!(store.chunk_count(), 10);

        // Page from the middle.
        let page = store.read_history(250, 5).unwrap();
        let rendered: Vec<String> = page.iter().map(StoredLine::render).collect();
        assert_eq!(
            rendered,
            vec!["line-250", "line-251", "line-252", "line-253", "line-254"]
        );

        // Read the very first line and past-the-end.
        assert_eq!(store.read_history(0, 1).unwrap()[0].render(), "line-0");
        assert!(store.read_history(5000, 10).unwrap().is_empty());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pending_tail_is_readable_before_flush() {
        let dir = tmp("tail");
        let mut store = ChunkStore::new(&dir, 100).unwrap();
        store.append([text("a"), text("b"), text("c")]).unwrap();
        // No full chunk yet, but the tail is still readable.
        assert_eq!(store.chunk_count(), 0);
        let page = store.read_history(1, 5).unwrap();
        assert_eq!(
            page.iter().map(StoredLine::render).collect::<Vec<_>>(),
            vec!["b", "c"]
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compression_shrinks_repetitive_history() {
        let dir = tmp("compress");
        let mut store = ChunkStore::new(&dir, 10_000).unwrap();
        for _ in 0..10_000 {
            store
                .append([text("the same repetitive log line over and over")])
                .unwrap();
        }
        store.flush().unwrap();
        let file = &store.chunks[0].path;
        let compressed_len = fs::metadata(file).unwrap().len();
        // Raw is ~430 KB; zstd must compress the repetitive content dramatically.
        assert!(
            compressed_len < 50_000,
            "compressed size {compressed_len} too large"
        );
        fs::remove_dir_all(&dir).ok();
    }
}
