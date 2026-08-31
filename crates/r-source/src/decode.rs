use std::{error::Error, fmt, ops::Range, sync::Arc};

use crate::{DecodeMode, EncodingProfile, SourceEncoding};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OffsetBias {
    Left,
    Right,
}

/// A monotonic map between byte boundaries in the original and decoded data.
///
/// Exact mapping returns `None` inside a transformed unit. Biased mapping can
/// select either edge of that unit, which is useful for diagnostic ranges.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OffsetMap {
    boundaries: Arc<[(usize, usize)]>,
    original_len: usize,
    decoded_len: usize,
}

impl OffsetMap {
    fn new(boundaries: Vec<(usize, usize)>, original_len: usize, decoded_len: usize) -> Self {
        debug_assert_eq!(boundaries.first(), Some(&(0, 0)));
        debug_assert_eq!(boundaries.last(), Some(&(decoded_len, original_len)));
        Self {
            boundaries: boundaries.into(),
            original_len,
            decoded_len,
        }
    }

    pub fn original_len(&self) -> usize {
        self.original_len
    }

    pub fn decoded_len(&self) -> usize {
        self.decoded_len
    }

    pub fn decoded_to_original_exact(&self, decoded: usize) -> Option<usize> {
        if decoded > self.decoded_len {
            return None;
        }
        match self.boundaries.binary_search_by_key(&decoded, |&(d, _)| d) {
            Ok(index) => Some(self.boundaries[index].1),
            Err(index) => {
                let (left_decoded, left_original) = self.boundaries[index - 1];
                let (right_decoded, right_original) = self.boundaries[index];
                ((right_decoded - left_decoded) == (right_original - left_original))
                    .then_some(left_original + decoded - left_decoded)
            }
        }
    }

    pub fn original_to_decoded_exact(&self, original: usize) -> Option<usize> {
        if original > self.original_len {
            return None;
        }
        match self.boundaries.binary_search_by_key(&original, |&(_, o)| o) {
            Ok(index) => Some(self.boundaries[index].0),
            Err(index) => {
                let (left_decoded, left_original) = self.boundaries[index - 1];
                let (right_decoded, right_original) = self.boundaries[index];
                ((right_decoded - left_decoded) == (right_original - left_original))
                    .then_some(left_decoded + original - left_original)
            }
        }
    }

    pub fn decoded_to_original(&self, decoded: usize, bias: OffsetBias) -> Option<usize> {
        if decoded > self.decoded_len {
            return None;
        }
        match self.boundaries.binary_search_by_key(&decoded, |&(d, _)| d) {
            Ok(index) => Some(self.boundaries[index].1),
            Err(index) => self
                .decoded_to_original_exact(decoded)
                .or_else(|| match bias {
                    OffsetBias::Left => Some(self.boundaries[index - 1].1),
                    OffsetBias::Right => Some(self.boundaries[index].1),
                }),
        }
    }

    pub fn original_to_decoded(&self, original: usize, bias: OffsetBias) -> Option<usize> {
        if original > self.original_len {
            return None;
        }
        match self.boundaries.binary_search_by_key(&original, |&(_, o)| o) {
            Ok(index) => Some(self.boundaries[index].0),
            Err(index) => self
                .original_to_decoded_exact(original)
                .or_else(|| match bias {
                    OffsetBias::Left => Some(self.boundaries[index - 1].0),
                    OffsetBias::Right => Some(self.boundaries[index].0),
                }),
        }
    }

    pub fn boundaries(&self) -> impl ExactSizeIterator<Item = (usize, usize)> + '_ {
        self.boundaries.iter().copied()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodeIssue {
    pub original_range: Range<usize>,
    pub decoded_range: Range<usize>,
}

#[derive(Clone, Debug)]
pub struct DecodedSource {
    text: Arc<str>,
    original: Arc<[u8]>,
    offsets: Arc<OffsetMap>,
    issues: Arc<[DecodeIssue]>,
    encoding: SourceEncoding,
}

impl DecodedSource {
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

    pub fn offset_map_arc(&self) -> Arc<OffsetMap> {
        Arc::clone(&self.offsets)
    }

    pub fn issues(&self) -> &[DecodeIssue] {
        &self.issues
    }

    pub fn encoding(&self) -> SourceEncoding {
        self.encoding
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodeError {
    InvalidUtf8 {
        valid_up_to: usize,
        error_len: Option<usize>,
    },
    DecodedTooLarge {
        limit: usize,
        actual: usize,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUtf8 { valid_up_to, .. } => {
                write!(f, "invalid UTF-8 at byte {valid_up_to}")
            }
            Self::DecodedTooLarge { limit, actual } => {
                write!(
                    f,
                    "decoded source is {actual} bytes, exceeding limit {limit}"
                )
            }
        }
    }
}

impl Error for DecodeError {}

/// Decodes bytes according to an explicit profile and enforces the decoded
/// UTF-8 byte limit. Original-size limits belong at the I/O-facing layer.
pub fn decode(
    original: impl Into<Arc<[u8]>>,
    profile: EncodingProfile,
    max_decoded_bytes: usize,
) -> Result<DecodedSource, DecodeError> {
    let original = original.into();
    match profile.encoding {
        SourceEncoding::Utf8 => decode_utf8(original, profile.mode, max_decoded_bytes),
        SourceEncoding::Latin1 => decode_latin1(original, max_decoded_bytes),
    }
}

fn decode_utf8(
    original: Arc<[u8]>,
    mode: DecodeMode,
    limit: usize,
) -> Result<DecodedSource, DecodeError> {
    if mode == DecodeMode::Strict {
        let text = std::str::from_utf8(&original).map_err(|error| DecodeError::InvalidUtf8 {
            valid_up_to: error.valid_up_to(),
            error_len: error.error_len(),
        })?;
        if text.len() > limit {
            return Err(DecodeError::DecodedTooLarge {
                limit,
                actual: text.len(),
            });
        }
        let boundaries = if original.is_empty() {
            vec![(0, 0)]
        } else {
            vec![(0, 0), (original.len(), original.len())]
        };
        return Ok(finish(
            text.into(),
            original,
            boundaries,
            Vec::new(),
            SourceEncoding::Utf8,
        ));
    }

    let mut text = String::with_capacity(original.len());
    let mut boundaries = vec![(0, 0)];
    let mut issues = Vec::new();
    let mut original_at = 0;
    while original_at < original.len() {
        match std::str::from_utf8(&original[original_at..]) {
            Ok(valid) => {
                append_identity(&mut text, &mut boundaries, valid, original_at);
                original_at = original.len();
            }
            Err(error) => {
                let valid_len = error.valid_up_to();
                if valid_len != 0 {
                    let valid =
                        std::str::from_utf8(&original[original_at..original_at + valid_len])
                            .expect("valid_up_to must delimit valid UTF-8");
                    append_identity(&mut text, &mut boundaries, valid, original_at);
                    original_at += valid_len;
                }
                let invalid_len = error.error_len().unwrap_or(original.len() - original_at);
                let decoded_start = text.len();
                text.push('\u{fffd}');
                let decoded_end = text.len();
                issues.push(DecodeIssue {
                    original_range: original_at..original_at + invalid_len,
                    decoded_range: decoded_start..decoded_end,
                });
                original_at += invalid_len;
                boundaries.push((decoded_end, original_at));
            }
        }
        if text.len() > limit {
            return Err(DecodeError::DecodedTooLarge {
                limit,
                actual: text.len(),
            });
        }
    }
    Ok(finish(
        text.into(),
        original,
        boundaries,
        issues,
        SourceEncoding::Utf8,
    ))
}

fn decode_latin1(original: Arc<[u8]>, limit: usize) -> Result<DecodedSource, DecodeError> {
    let mut text = String::with_capacity(original.len());
    let mut boundaries = vec![(0, 0)];
    for (index, byte) in original.iter().copied().enumerate() {
        if !byte.is_ascii() {
            push_boundary(&mut boundaries, text.len(), index);
        }
        text.push(char::from(byte));
        if !byte.is_ascii() {
            push_boundary(&mut boundaries, text.len(), index + 1);
        }
        if text.len() > limit {
            return Err(DecodeError::DecodedTooLarge {
                limit,
                actual: text.len(),
            });
        }
    }
    push_boundary(&mut boundaries, text.len(), original.len());
    Ok(finish(
        text.into(),
        original,
        boundaries,
        Vec::new(),
        SourceEncoding::Latin1,
    ))
}

fn append_identity(
    text: &mut String,
    boundaries: &mut Vec<(usize, usize)>,
    valid: &str,
    original_start: usize,
) {
    let decoded_start = text.len();
    text.push_str(valid);
    push_boundary(
        boundaries,
        decoded_start + valid.len(),
        original_start + valid.len(),
    );
}

fn push_boundary(boundaries: &mut Vec<(usize, usize)>, decoded: usize, original: usize) {
    if boundaries.last() != Some(&(decoded, original)) {
        boundaries.push((decoded, original));
    }
}

fn finish(
    text: Arc<str>,
    original: Arc<[u8]>,
    boundaries: Vec<(usize, usize)>,
    issues: Vec<DecodeIssue>,
    encoding: SourceEncoding,
) -> DecodedSource {
    let offsets = OffsetMap::new(boundaries, original.len(), text.len());
    DecodedSource {
        text,
        original,
        offsets: Arc::new(offsets),
        issues: issues.into(),
        encoding,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_utf8_retains_bytes_and_identity_offsets() {
        let decoded = decode(
            Arc::<[u8]>::from(&b"a\xc3\xa9"[..]),
            EncodingProfile::UTF8_STRICT,
            10,
        )
        .unwrap();
        assert_eq!(decoded.text(), "aé");
        assert_eq!(decoded.original_bytes(), b"a\xc3\xa9");
        assert_eq!(decoded.offset_map().decoded_to_original_exact(2), Some(2));
        assert!(decoded.issues().is_empty());
    }

    #[test]
    fn strict_utf8_reports_precise_error() {
        let error = decode(
            Arc::<[u8]>::from(&b"ok\xff"[..]),
            EncodingProfile::UTF8_STRICT,
            usize::MAX,
        )
        .unwrap_err();
        assert_eq!(
            error,
            DecodeError::InvalidUtf8 {
                valid_up_to: 2,
                error_len: Some(1)
            }
        );
    }

    #[test]
    fn recovering_utf8_maps_replacement_edges() {
        let decoded = decode(
            Arc::<[u8]>::from(&b"a\xf0\x28\x8c\x28z"[..]),
            EncodingProfile::UTF8_RECOVERING,
            usize::MAX,
        )
        .unwrap();
        assert_eq!(decoded.text(), "a�(�(z");
        assert_eq!(decoded.issues().len(), 2);
        assert_eq!(decoded.issues()[0].original_range, 1..2);
        assert_eq!(decoded.offset_map().decoded_to_original_exact(2), None);
        assert_eq!(
            decoded
                .offset_map()
                .decoded_to_original(2, OffsetBias::Left),
            Some(1)
        );
        assert_eq!(
            decoded
                .offset_map()
                .decoded_to_original(2, OffsetBias::Right),
            Some(2)
        );
        assert_eq!(decoded.offset_map().original_to_decoded_exact(1), Some(1));
    }

    #[test]
    fn recovering_incomplete_sequence_is_one_issue() {
        let decoded = decode(
            Arc::<[u8]>::from(&b"x\xe2\x82"[..]),
            EncodingProfile::UTF8_RECOVERING,
            usize::MAX,
        )
        .unwrap();
        assert_eq!(decoded.text(), "x�");
        assert_eq!(decoded.issues()[0].original_range, 1..3);
        assert_eq!(
            decoded
                .offset_map()
                .original_to_decoded(2, OffsetBias::Right),
            Some(4)
        );
    }

    #[test]
    fn latin1_expands_and_maps_high_bytes() {
        let decoded = decode(
            Arc::<[u8]>::from(&b"A\xe9B"[..]),
            EncodingProfile::LATIN1,
            usize::MAX,
        )
        .unwrap();
        assert_eq!(decoded.text(), "AéB");
        assert_eq!(decoded.offset_map().decoded_to_original_exact(2), None);
        assert_eq!(
            decoded
                .offset_map()
                .decoded_to_original(2, OffsetBias::Left),
            Some(1)
        );
        assert_eq!(
            decoded
                .offset_map()
                .decoded_to_original(2, OffsetBias::Right),
            Some(2)
        );
    }

    #[test]
    fn decoded_limit_is_enforced_after_expansion() {
        let error =
            decode(Arc::<[u8]>::from(&b"\xff"[..]), EncodingProfile::LATIN1, 1).unwrap_err();
        assert_eq!(
            error,
            DecodeError::DecodedTooLarge {
                limit: 1,
                actual: 2
            }
        );
    }
}
