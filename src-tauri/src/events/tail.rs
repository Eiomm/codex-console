use std::{fs::File, io::{self, Read, Seek, SeekFrom}, path::Path};
pub const MAX_LINE: usize = 16 * 1024 * 1024;
const READ_BUDGET: u64 = 1024 * 1024;
#[derive(Debug, Default)]
pub struct TailBatch { pub lines: Vec<String>, pub skipped: u64, pub reset: bool, pub has_more: bool }
/// Byte framing only. Parsers own their state and are reset by the caller on rotation/truncation.
#[derive(Default)]
pub struct Tailer {
    offset: u64,
    pending: Vec<u8>,
    dropping: bool,
    identity: Option<(u64, u64)>,
}
impl Tailer {
    pub fn new() -> Self { Self::default() }
    pub fn poll(&mut self, path: &Path) -> io::Result<TailBatch> {
        let mut file = File::open(path)?;
        let meta = file.metadata()?;
        #[cfg(unix)]
        let identity = { use std::os::unix::fs::MetadataExt; (meta.dev(), meta.ino()) };
        #[cfg(not(unix))]
        let identity = (0, 0);
        let mut batch = TailBatch::default();
        if self.offset > meta.len() || self.identity.is_some_and(|old| old != identity) {
            self.offset = 0; self.pending.clear(); self.dropping = false; batch.reset = true;
        }
        self.identity = Some(identity);
        file.seek(SeekFrom::Start(self.offset))?;
        // Bound each poll as well as each line; a continuously growing writer cannot monopolize the worker.
        let end = meta.len().min(self.offset.saturating_add(READ_BUDGET));
        let mut buf = [0u8; 64 * 1024];
        while self.offset < end {
            let max = (end - self.offset).min(buf.len() as u64) as usize;
            let len = file.read(&mut buf[..max])?;
            if len == 0 { break; }
            self.offset += len as u64;
            for &byte in &buf[..len] {
                if byte == b'\n' {
                    if !self.dropping {
                        if self.pending.last() == Some(&b'\r') { self.pending.pop(); }
                        batch.lines.push(String::from_utf8_lossy(&self.pending).into_owned());
                    }
                    self.pending.clear(); self.dropping = false;
                } else if !self.dropping {
                    if self.pending.len() == MAX_LINE {
                        self.pending.clear(); self.dropping = true; batch.skipped += 1;
                    } else { self.pending.push(byte); }
                }
            }
        }
        batch.has_more = self.offset < meta.len();
        Ok(batch)
    }
    /// Used only by snapshot loading, where a valid final JSON line need not end in LF.
    pub fn finish(&mut self) -> Option<String> {
        if self.dropping || self.pending.is_empty() { return None; }
        Some(String::from_utf8_lossy(&std::mem::take(&mut self.pending)).into_owned())
    }
}
