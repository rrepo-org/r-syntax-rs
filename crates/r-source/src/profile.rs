use std::fmt;

/// A pinned R language version. It is data, not a probe of an installed R.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub struct RVersion {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

impl RVersion {
    pub const R_4_6_1: Self = Self::new(4, 6, 1);

    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for RVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Language behavior selected independently of the host system.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CompatibilityProfile {
    pub r_version: RVersion,
    pub recognize_line_directives: bool,
}

impl CompatibilityProfile {
    pub const R_4_6_1: Self = Self {
        r_version: RVersion::R_4_6_1,
        recognize_line_directives: true,
    };
}

impl Default for CompatibilityProfile {
    fn default() -> Self {
        Self::R_4_6_1
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SourceEncoding {
    Utf8,
    Latin1,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DecodeMode {
    Strict,
    Recovering,
}

/// Input decoding choices. The default recovers malformed UTF-8 so parsers can
/// still produce a tree; strict decoding remains available at every entrypoint.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EncodingProfile {
    pub encoding: SourceEncoding,
    pub mode: DecodeMode,
}

impl EncodingProfile {
    pub const UTF8_STRICT: Self = Self::new(SourceEncoding::Utf8, DecodeMode::Strict);
    pub const UTF8_RECOVERING: Self = Self::new(SourceEncoding::Utf8, DecodeMode::Recovering);
    pub const LATIN1: Self = Self::new(SourceEncoding::Latin1, DecodeMode::Strict);

    pub const fn new(encoding: SourceEncoding, mode: DecodeMode) -> Self {
        Self { encoding, mode }
    }
}

impl Default for EncodingProfile {
    fn default() -> Self {
        Self::UTF8_RECOVERING
    }
}

/// Locale-sensitive choices needed by syntax processing, pinned as plain data.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LocaleProfile {
    pub name: &'static str,
    pub decimal_mark: char,
    pub encoding: EncodingProfile,
}

impl LocaleProfile {
    pub const R_4_6_1: Self = Self {
        name: "C.UTF-8",
        decimal_mark: '.',
        encoding: EncodingProfile::UTF8_RECOVERING,
    };
}

impl Default for LocaleProfile {
    fn default() -> Self {
        Self::R_4_6_1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_pinned() {
        assert_eq!(
            CompatibilityProfile::default().r_version,
            RVersion::new(4, 6, 1)
        );
        assert_eq!(LocaleProfile::default().name, "C.UTF-8");
        assert_eq!(EncodingProfile::default().mode, DecodeMode::Recovering);
        assert_eq!(RVersion::R_4_6_1.to_string(), "4.6.1");
    }
}
