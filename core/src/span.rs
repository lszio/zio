use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// A byte position in source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BytePos(pub usize);

/// A source location: byte range + line/col for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    /// Source file identifier.
    pub source_id: SourceId,
    /// Byte offset of the start of this span (inclusive).
    pub start: BytePos,
    /// Byte offset of the end of this span (exclusive).
    pub end: BytePos,
    /// 1-indexed line number.
    pub line: usize,
    /// 1-indexed column number (byte offset from line start).
    pub col: usize,
}

impl Span {
    /// A dummy span used for synthetic / generated code.
    pub const DUMMY: Span = Span {
        source_id: SourceId(0),
        start: BytePos(0),
        end: BytePos(0),
        line: 0,
        col: 0,
    };

    /// Create a new span from a source file and byte range.
    /// The line/col are lazily resolved from the SourceMap.
    pub fn new(source_id: SourceId, start: BytePos, end: BytePos, line: usize, col: usize) -> Self {
        Span { source_id, start, end, line, col }
    }

    /// Format a human-readable location like `file.zio:12:5`.
    pub fn display(&self, source_map: &SourceMap) -> String {
        let name = source_map.source_name(self.source_id);
        format!("{}:{}:{}", name, self.line, self.col)
    }

    /// Check if this is a dummy (synthetic) span.
    pub fn is_dummy(&self) -> bool {
        self.source_id.0 == 0 && self.start.0 == 0 && self.end.0 == 0
    }
}

/// A unique identifier for a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceId(pub usize);

impl SourceId {
    pub const NONE: SourceId = SourceId(0);
}

/// A registered source file or REPL input.
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub id: SourceId,
    pub name: String,
    pub source: String,
    /// Pre-computed line start byte offsets for fast line→offset lookup.
    line_starts: Vec<BytePos>,
}

impl SourceFile {
    pub fn new(id: SourceId, name: String, source: String) -> Self {
        let line_starts = compute_line_starts(&source);
        SourceFile { id, name, source, line_starts }
    }

    /// Given a byte offset, return (line_number, col_number) — both 1-indexed.
    pub fn line_col(&self, pos: BytePos) -> (usize, usize) {
        let offset = pos.0.min(self.source.len());
        match self.line_starts.binary_search(&BytePos(offset)) {
            Ok(line_idx) => (line_idx + 1, 1),
            Err(line_idx) => {
                if line_idx == 0 {
                    (1, offset + 1)
                } else {
                    let line_start = self.line_starts[line_idx - 1].0;
                    (line_idx, offset - line_start + 1)
                }
            }
        }
    }

    /// Get the source line at the given 1-indexed line number.
    pub fn get_line(&self, line: usize) -> Option<&str> {
        if line == 0 || line > self.line_starts.len() {
            return None;
        }
        let start = self.line_starts[line - 1].0;
        let end = if line < self.line_starts.len() {
            self.line_starts[line].0
        } else {
            self.source.len()
        };
        // Strip trailing \n or \r\n
        let end = if self.source.as_bytes().get(end.saturating_sub(1)) == Some(&b'\n') {
            end - 1
        } else {
            end
        };
        let end = if self.source.as_bytes().get(end.saturating_sub(1)) == Some(&b'\r') {
            end - 1
        } else {
            end
        };
        if start > end {
            return Some("");
        }
        Some(&self.source[start..end])
    }

    /// Create a Span for the given byte range in this file.
    pub fn span(&self, start: BytePos, end: BytePos) -> Span {
        let (line, col) = self.line_col(start);
        Span::new(self.id, start, end, line, col)
    }
}

fn compute_line_starts(source: &str) -> Vec<BytePos> {
    let mut starts = vec![BytePos(0)];
    for (i, c) in source.char_indices() {
        if c == '\n' {
            starts.push(BytePos(i + 1));
        }
    }
    starts
}

/// Global registry of source files, allowing lazy line/col resolution.
#[derive(Debug)]
pub struct SourceMap {
    files: Mutex<Vec<Option<SourceFile>>>,
    next_id: AtomicUsize,
}

impl SourceMap {
    pub fn new() -> Self {
        let mut files = Vec::new();
        // ID 0 is reserved for "no source" / synthetic
        files.push(None);
        SourceMap {
            files: Mutex::new(files),
            next_id: AtomicUsize::new(1),
        }
    }

    /// Register a new source file and return its ID.
    pub fn register(&self, name: String, source: String) -> SourceId {
        let id = SourceId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let file = SourceFile::new(id, name, source);
        let mut files = self.files.lock().unwrap();
        while files.len() <= id.0 {
            files.push(None);
        }
        files[id.0] = Some(file);
        id
    }

    /// Look up a source file by ID, returning a guard.
    pub fn get(&self, id: SourceId) -> Option<SourceFile> {
        let files = self.files.lock().unwrap();
        files.get(id.0)?.clone()
    }

    /// Get the source name for display.
    pub fn source_name(&self, id: SourceId) -> String {
        let files = self.files.lock().unwrap();
        files
            .get(id.0)
            .and_then(|f| f.as_ref())
            .map(|f| f.name.clone())
            .unwrap_or_else(|| format!("<source {}>", id.0))
    }

    /// Resolve a span to a human-readable location string.
    pub fn span_display(&self, span: Span) -> String {
        if span.is_dummy() {
            return "<synthetic>".into();
        }
        let name = self.source_name(span.source_id);
        format!("{}:{}:{}", name, span.line, span.col)
    }

    /// Return the source line context for an error message.
    pub fn span_context(&self, span: Span) -> Option<String> {
        let files = self.files.lock().unwrap();
        let file = files.get(span.source_id.0)?.as_ref()?;
        let line = file.get_line(span.line)?;
        let indicator = format!("{:>width$}^-- here", "", width = span.col);
        Some(format!("  |\n  | {line}\n  | {indicator}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_byte_pos() {
        let p1 = BytePos(0);
        let p2 = BytePos(10);
        assert_ne!(p1, p2);
        assert!(p1 < p2);
    }

    #[test]
    fn test_span_display() {
        let map = SourceMap::new();
        let id = map.register("test.zio".into(), "(+ 1 2)".into());
        let span = Span::new(id, BytePos(0), BytePos(7), 1, 1);
        assert_eq!(map.span_display(span), "test.zio:1:1");
    }

    #[test]
    fn test_source_file_line_col() {
        let source = "line1\nline2\nline3\n";
        let file = SourceFile::new(SourceId(1), "test.zio".into(), source.into());

        assert_eq!(file.line_col(BytePos(0)), (1, 1));   // 'l' of line1
        assert_eq!(file.line_col(BytePos(5)), (1, 6));   // '\n' of line1
        assert_eq!(file.line_col(BytePos(6)), (2, 1));   // 'l' of line2
        assert_eq!(file.line_col(BytePos(11)), (2, 6));  // '\n' of line2
        assert_eq!(file.line_col(BytePos(12)), (3, 1));  // 'l' of line3
    }

    #[test]
    fn test_get_line() {
        let source = "first\nsecond\nthird\n";
        let file = SourceFile::new(SourceId(1), "t.zio".into(), source.into());
        assert_eq!(file.get_line(1), Some("first"));
        assert_eq!(file.get_line(2), Some("second"));
        assert_eq!(file.get_line(3), Some("third"));
        assert_eq!(file.get_line(4), Some(""));

        // No trailing newline
        let file2 = SourceFile::new(SourceId(2), "t.zio".into(), "hello".into());
        assert_eq!(file2.get_line(1), Some("hello"));
    }

    #[test]
    fn test_source_map_register() {
        let map = SourceMap::new();
        let id = map.register("hello.zio".into(), "(print 42)".into());
        assert_ne!(id, SourceId::NONE);

        let name = map.source_name(id);
        assert_eq!(name, "hello.zio");
    }

    #[test]
    fn test_span_annotation() {
        let map = SourceMap::new();
        let id = map.register("test.zio".into(), "(+ 1 2)\n".into());
        // Span covering "+ 1"
        let span = Span::new(id, BytePos(1), BytePos(5), 1, 2);
        let ctx = map.span_context(span);
        assert!(ctx.is_some());
        let ctx_str = ctx.unwrap();
        assert!(ctx_str.contains("(+ 1 2)"));
        assert!(ctx_str.contains("^-- here"));
    }
}