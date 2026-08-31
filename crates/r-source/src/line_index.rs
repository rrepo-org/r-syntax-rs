use std::{error::Error, fmt, ops::Range, sync::Arc};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TextRange {
    pub start: usize,
    pub end: usize,
}

impl TextRange {
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub const fn len(self) -> usize {
        self.end - self.start
    }

    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

impl From<TextRange> for Range<usize> {
    fn from(value: TextRange) -> Self {
        value.start..value.end
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LinePosition {
    /// Zero-based physical line.
    pub line: usize,
    /// UTF-8 byte column.
    pub utf8_column: usize,
    /// Unicode scalar-value column.
    pub scalar_column: usize,
    /// UTF-16 code-unit column (for LSP-compatible coordinates).
    pub utf16_column: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PositionError {
    OffsetOutOfBounds { offset: usize, len: usize },
    LineOutOfBounds { line: usize, line_count: usize },
    ColumnOutOfBounds { line: usize, column: usize },
    NotUtf8Boundary { offset: usize },
    InsideUtf16Scalar { line: usize, column: usize },
}

impl fmt::Display for PositionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OffsetOutOfBounds { offset, len } => {
                write!(f, "offset {offset} is outside source of length {len}")
            }
            Self::LineOutOfBounds { line, line_count } => {
                write!(f, "line {line} is outside {line_count} lines")
            }
            Self::ColumnOutOfBounds { line, column } => {
                write!(f, "column {column} is outside line {line}")
            }
            Self::NotUtf8Boundary { offset } => {
                write!(f, "offset {offset} is not a UTF-8 boundary")
            }
            Self::InsideUtf16Scalar { line, column } => {
                write!(
                    f,
                    "UTF-16 column {column} is inside a scalar on line {line}"
                )
            }
        }
    }
}

impl Error for PositionError {}

/// An index of physical lines in decoded UTF-8 text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineIndex {
    text: Arc<str>,
    starts: Arc<[usize]>,
}

impl LineIndex {
    pub fn new(text: impl Into<Arc<str>>) -> Self {
        let text = text.into();
        let bytes = text.as_bytes();
        let mut starts = vec![0];
        let mut offset = 0;
        while offset < bytes.len() {
            match bytes[offset] {
                b'\r' => {
                    offset += 1;
                    if bytes.get(offset) == Some(&b'\n') {
                        offset += 1;
                    }
                    starts.push(offset);
                }
                b'\n' => {
                    offset += 1;
                    starts.push(offset);
                }
                _ => offset += 1,
            }
        }
        Self {
            text,
            starts: starts.into(),
        }
    }

    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    pub fn text_len(&self) -> usize {
        self.text.len()
    }

    pub fn line_start(&self, line: usize) -> Option<usize> {
        self.starts.get(line).copied()
    }

    pub fn line_of_offset(&self, offset: usize) -> Result<usize, PositionError> {
        if offset > self.text.len() {
            return Err(PositionError::OffsetOutOfBounds {
                offset,
                len: self.text.len(),
            });
        }
        Ok(self.starts.partition_point(|&start| start <= offset) - 1)
    }

    /// Full line range, including its CR, LF, or CRLF terminator.
    pub fn line_full_range(&self, line: usize) -> Result<TextRange, PositionError> {
        let start = self.checked_line_start(line)?;
        let end = self
            .starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.text.len());
        Ok(TextRange::new(start, end))
    }

    /// Line range excluding its terminator.
    pub fn line_range(&self, line: usize) -> Result<TextRange, PositionError> {
        let full = self.line_full_range(line)?;
        let bytes = self.text.as_bytes();
        let mut end = full.end;
        if end > full.start && bytes[end - 1] == b'\n' {
            end -= 1;
        }
        if end > full.start && bytes[end - 1] == b'\r' {
            end -= 1;
        }
        Ok(TextRange::new(full.start, end))
    }

    pub fn position(&self, offset: usize) -> Result<LinePosition, PositionError> {
        let line = self.line_of_offset(offset)?;
        if !self.text.is_char_boundary(offset) {
            return Err(PositionError::NotUtf8Boundary { offset });
        }
        let start = self.starts[line];
        let prefix = &self.text[start..offset];
        Ok(LinePosition {
            line,
            utf8_column: offset - start,
            scalar_column: prefix.chars().count(),
            utf16_column: prefix.encode_utf16().count(),
        })
    }

    pub fn offset_utf8(&self, line: usize, column: usize) -> Result<usize, PositionError> {
        let range = self.line_range(line)?;
        let offset = range
            .start
            .checked_add(column)
            .filter(|&value| value <= range.end)
            .ok_or(PositionError::ColumnOutOfBounds { line, column })?;
        if !self.text.is_char_boundary(offset) {
            return Err(PositionError::NotUtf8Boundary { offset });
        }
        Ok(offset)
    }

    pub fn offset_scalar(&self, line: usize, column: usize) -> Result<usize, PositionError> {
        let range = self.line_range(line)?;
        if column == 0 {
            return Ok(range.start);
        }
        self.text[range.start..range.end]
            .char_indices()
            .nth(column)
            .map(|(relative, _)| range.start + relative)
            .or_else(|| {
                (self.text[range.start..range.end].chars().count() == column).then_some(range.end)
            })
            .ok_or(PositionError::ColumnOutOfBounds { line, column })
    }

    pub fn offset_utf16(&self, line: usize, column: usize) -> Result<usize, PositionError> {
        let range = self.line_range(line)?;
        let mut units = 0;
        for (relative, character) in self.text[range.start..range.end].char_indices() {
            if units == column {
                return Ok(range.start + relative);
            }
            let next = units + character.len_utf16();
            if column < next {
                return Err(PositionError::InsideUtf16Scalar { line, column });
            }
            units = next;
        }
        if units == column {
            Ok(range.end)
        } else {
            Err(PositionError::ColumnOutOfBounds { line, column })
        }
    }

    pub fn line_text(&self, line: usize) -> Result<&str, PositionError> {
        let range = self.line_range(line)?;
        Ok(&self.text[range.start..range.end])
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    fn checked_line_start(&self, line: usize) -> Result<usize, PositionError> {
        self.line_start(line).ok_or(PositionError::LineOutOfBounds {
            line,
            line_count: self.line_count(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_all_physical_newline_forms() {
        let index = LineIndex::new(Arc::<str>::from("a\r\nb\rc\nd"));
        assert_eq!(index.line_count(), 4);
        assert_eq!(index.line_range(0).unwrap(), TextRange::new(0, 1));
        assert_eq!(index.line_full_range(0).unwrap(), TextRange::new(0, 3));
        assert_eq!(index.line_text(1).unwrap(), "b");
        assert_eq!(index.line_text(2).unwrap(), "c");
        assert_eq!(index.line_text(3).unwrap(), "d");
    }

    #[test]
    fn trailing_newline_creates_empty_physical_line() {
        let index = LineIndex::new(Arc::<str>::from("x\n"));
        assert_eq!(index.line_count(), 2);
        assert_eq!(index.line_text(1).unwrap(), "");
        assert_eq!(index.line_of_offset(2).unwrap(), 1);
    }

    #[test]
    fn converts_unicode_columns_both_ways() {
        let index = LineIndex::new(Arc::<str>::from("a😀é\n"));
        let position = index.position(7).unwrap();
        assert_eq!(
            position,
            LinePosition {
                line: 0,
                utf8_column: 7,
                scalar_column: 3,
                utf16_column: 4,
            }
        );
        assert_eq!(index.offset_utf8(0, 5).unwrap(), 5);
        assert_eq!(index.offset_scalar(0, 2).unwrap(), 5);
        assert_eq!(index.offset_utf16(0, 3).unwrap(), 5);
        assert_eq!(
            index.offset_utf16(0, 2),
            Err(PositionError::InsideUtf16Scalar { line: 0, column: 2 })
        );
        assert_eq!(
            index.offset_utf8(0, 2),
            Err(PositionError::NotUtf8Boundary { offset: 2 })
        );
    }

    #[test]
    fn rejects_out_of_range_coordinates() {
        let index = LineIndex::new(Arc::<str>::from("abc"));
        assert!(matches!(
            index.position(4),
            Err(PositionError::OffsetOutOfBounds { .. })
        ));
        assert!(matches!(
            index.line_range(1),
            Err(PositionError::LineOutOfBounds { .. })
        ));
        assert!(matches!(
            index.offset_scalar(0, 4),
            Err(PositionError::ColumnOutOfBounds { .. })
        ));
    }
}
