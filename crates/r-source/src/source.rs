use std::{error::Error, fmt, sync::Arc};

use crate::{
    decode, CompatibilityProfile, DecodeError, DecodeIssue, EncodingProfile, LineIndex,
    LogicalSourceMap, OffsetMap, SourceEncoding,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SourceLimits {
    pub max_original_bytes: usize,
    pub max_decoded_bytes: usize,
}

impl SourceLimits {
    pub const DEFAULT: Self = Self::new(64 * 1024 * 1024, 128 * 1024 * 1024);
    pub const UNLIMITED: Self = Self::new(usize::MAX, usize::MAX);

    pub const fn new(max_original_bytes: usize, max_decoded_bytes: usize) -> Self {
        Self {
            max_original_bytes,
            max_decoded_bytes,
        }
    }
}

impl Default for SourceLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceError {
    OriginalTooLarge { limit: usize, actual: usize },
    Decode(DecodeError),
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OriginalTooLarge { limit, actual } => {
                write!(f, "source is {actual} bytes, exceeding limit {limit}")
            }
            Self::Decode(error) => error.fmt(f),
        }
    }
}

impl Error for SourceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Decode(error) => Some(error),
            Self::OriginalTooLarge { .. } => None,
        }
    }
}

impl From<DecodeError> for SourceError {
    fn from(value: DecodeError) -> Self {
        Self::Decode(value)
    }
}

/// A cheap-to-clone immutable source snapshot and all of its coordinate maps.
#[derive(Clone, Debug)]
pub struct SourceText {
    text: Arc<str>,
    original: Arc<[u8]>,
    offsets: Arc<OffsetMap>,
    issues: Arc<[DecodeIssue]>,
    lines: Arc<LineIndex>,
    logical: Arc<LogicalSourceMap>,
    source_name: Option<Arc<str>>,
    compatibility: CompatibilityProfile,
    encoding: EncodingProfile,
}

impl SourceText {
    pub fn from_bytes(
        bytes: impl Into<Arc<[u8]>>,
        source_name: Option<Arc<str>>,
        compatibility: CompatibilityProfile,
        encoding: EncodingProfile,
        limits: SourceLimits,
    ) -> Result<Self, SourceError> {
        let original = bytes.into();
        if original.len() > limits.max_original_bytes {
            return Err(SourceError::OriginalTooLarge {
                limit: limits.max_original_bytes,
                actual: original.len(),
            });
        }
        let decoded = decode(original, encoding, limits.max_decoded_bytes)?;
        let text = decoded.text_arc();
        let lines = Arc::new(LineIndex::new(Arc::clone(&text)));
        let logical = if compatibility.recognize_line_directives {
            LogicalSourceMap::parse(&lines, source_name.clone())
        } else {
            LogicalSourceMap::identity(lines.line_count(), source_name.clone())
        };
        Ok(Self {
            text,
            original: decoded.original_bytes_arc(),
            offsets: decoded.offset_map_arc(),
            issues: decoded.issues().into(),
            lines,
            logical: Arc::new(logical),
            source_name,
            compatibility,
            encoding,
        })
    }

    /// Creates a source from known-valid UTF-8 using the pinned R 4.6.1 profile.
    pub fn new(text: impl AsRef<str>, source_name: Option<Arc<str>>) -> Self {
        let bytes: Arc<[u8]> = Arc::from(text.as_ref().as_bytes());
        Self::from_bytes(
            bytes,
            source_name,
            CompatibilityProfile::default(),
            EncodingProfile::UTF8_STRICT,
            SourceLimits::UNLIMITED,
        )
        .expect("a Rust string is valid UTF-8")
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn text_arc(&self) -> Arc<str> {
        Arc::clone(&self.text)
    }

    pub fn original_bytes(&self) -> &[u8] {
        &self.original
    }

    pub fn original_bytes_arc(&self) -> Arc<[u8]> {
        Arc::clone(&self.original)
    }

    pub fn offset_map(&self) -> &OffsetMap {
        &self.offsets
    }

    pub fn issues(&self) -> &[DecodeIssue] {
        &self.issues
    }

    pub fn line_index(&self) -> &LineIndex {
        &self.lines
    }

    pub fn line_index_arc(&self) -> Arc<LineIndex> {
        Arc::clone(&self.lines)
    }

    pub fn logical_map(&self) -> &LogicalSourceMap {
        &self.logical
    }

    pub fn logical_map_arc(&self) -> Arc<LogicalSourceMap> {
        Arc::clone(&self.logical)
    }

    pub fn source_name(&self) -> Option<&str> {
        self.source_name.as_deref()
    }

    pub fn compatibility_profile(&self) -> CompatibilityProfile {
        self.compatibility
    }

    pub fn encoding_profile(&self) -> EncodingProfile {
        self.encoding
    }

    pub fn source_encoding(&self) -> SourceEncoding {
        self.encoding.encoding
    }
}

impl AsRef<str> for SourceText {
    fn as_ref(&self) -> &str {
        self.text()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DecodeMode, LogicalLocation};

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn public_snapshots_are_send_and_sync() {
        assert_send_sync::<SourceText>();
        assert_send_sync::<LineIndex>();
        assert_send_sync::<LogicalSourceMap>();
        assert_send_sync::<OffsetMap>();
        assert_send_sync::<LogicalLocation>();
    }

    #[test]
    fn source_composes_decoding_and_maps() {
        let source = SourceText::from_bytes(
            Arc::<[u8]>::from(&b"#line 10 \"gen.R\"\n\xff\n"[..]),
            Some(Arc::from("input.R")),
            CompatibilityProfile::default(),
            EncodingProfile::new(SourceEncoding::Utf8, DecodeMode::Recovering),
            SourceLimits::UNLIMITED,
        )
        .unwrap();
        assert_eq!(source.original_bytes()[17], 0xff);
        assert_eq!(source.text(), "#line 10 \"gen.R\"\n�\n");
        assert_eq!(source.issues().len(), 1);
        let location = source.logical_map().location(1, 0).unwrap();
        assert_eq!(location.line, 10);
        assert_eq!(&*location.source_name.unwrap(), "gen.R");
    }

    #[test]
    fn original_limit_precedes_decoding() {
        let error = SourceText::from_bytes(
            Arc::<[u8]>::from(&b"abc"[..]),
            None,
            CompatibilityProfile::default(),
            EncodingProfile::UTF8_STRICT,
            SourceLimits::new(2, 10),
        )
        .unwrap_err();
        assert_eq!(
            error,
            SourceError::OriginalTooLarge {
                limit: 2,
                actual: 3
            }
        );
    }

    #[test]
    fn compatibility_can_disable_directives() {
        let source = SourceText::from_bytes(
            Arc::<[u8]>::from(&b"#line 20\nx"[..]),
            None,
            CompatibilityProfile {
                recognize_line_directives: false,
                ..CompatibilityProfile::default()
            },
            EncodingProfile::UTF8_STRICT,
            SourceLimits::UNLIMITED,
        )
        .unwrap();
        assert_eq!(source.logical_map().location(1, 0).unwrap().line, 2);
    }
}
