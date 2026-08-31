use std::{error::Error, fmt, sync::Arc};

use crate::LineIndex;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LineDirective {
    /// Zero-based physical line containing `#line`.
    pub physical_line: usize,
    /// One-based logical line assigned to the following physical line.
    pub logical_line: u32,
    /// A replacement source name; `None` preserves the current name.
    pub source_name: Option<Arc<str>>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LogicalLocation {
    pub source_name: Option<Arc<str>>,
    /// One-based logical line.
    pub line: u32,
    /// Zero-based column in the caller's chosen coordinate system.
    pub column: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LogicalSourceMapError {
    DirectiveLineOutOfBounds { line: usize, line_count: usize },
    ZeroLogicalLine { physical_line: usize },
    DirectivesOutOfOrder,
}

impl fmt::Display for LogicalSourceMapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DirectiveLineOutOfBounds { line, line_count } => {
                write!(
                    f,
                    "#line directive on physical line {line} is outside {line_count} lines"
                )
            }
            Self::ZeroLogicalLine { physical_line } => {
                write!(
                    f,
                    "#line on physical line {physical_line} specifies line zero"
                )
            }
            Self::DirectivesOutOfOrder => write!(f, "#line directives are not in physical order"),
        }
    }
}

impl Error for LogicalSourceMapError {}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Segment {
    physical_start: usize,
    logical_start: u32,
    source_name: Option<Arc<str>>,
}

/// Maps zero-based physical lines to one-based logical source locations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogicalSourceMap {
    line_count: usize,
    directives: Arc<[LineDirective]>,
    segments: Arc<[Segment]>,
}

impl LogicalSourceMap {
    pub fn identity(line_count: usize, source_name: Option<Arc<str>>) -> Self {
        Self {
            line_count,
            directives: Arc::from([]),
            segments: Arc::from([Segment {
                physical_start: 0,
                logical_start: 1,
                source_name,
            }]),
        }
    }

    pub fn from_directives(
        line_count: usize,
        source_name: Option<Arc<str>>,
        directives: impl IntoIterator<Item = LineDirective>,
    ) -> Result<Self, LogicalSourceMapError> {
        let directives: Vec<_> = directives.into_iter().collect();
        let mut segments = vec![Segment {
            physical_start: 0,
            logical_start: 1,
            source_name: source_name.clone(),
        }];
        let mut current_name = source_name;
        let mut previous = None;
        for directive in &directives {
            if directive.physical_line >= line_count {
                return Err(LogicalSourceMapError::DirectiveLineOutOfBounds {
                    line: directive.physical_line,
                    line_count,
                });
            }
            if directive.logical_line == 0 {
                return Err(LogicalSourceMapError::ZeroLogicalLine {
                    physical_line: directive.physical_line,
                });
            }
            if previous.is_some_and(|line| directive.physical_line <= line) {
                return Err(LogicalSourceMapError::DirectivesOutOfOrder);
            }
            previous = Some(directive.physical_line);
            if let Some(name) = &directive.source_name {
                current_name = Some(Arc::clone(name));
            }
            segments.push(Segment {
                physical_start: directive.physical_line + 1,
                logical_start: directive.logical_line,
                source_name: current_name.clone(),
            });
        }
        Ok(Self {
            line_count,
            directives: directives.into(),
            segments: segments.into(),
        })
    }

    /// Scans syntactically valid `#line N` and `#line N "source"` directives.
    /// Leading horizontal whitespace and whitespace after `#` are accepted.
    pub fn parse(index: &LineIndex, source_name: Option<Arc<str>>) -> Self {
        let directives = (0..index.line_count()).filter_map(|physical_line| {
            parse_directive(index.line_text(physical_line).ok()?, physical_line)
        });
        Self::from_directives(index.line_count(), source_name, directives)
            .expect("parsed directives are ordered and valid")
    }

    pub fn line_count(&self) -> usize {
        self.line_count
    }

    pub fn directives(&self) -> &[LineDirective] {
        &self.directives
    }

    pub fn location(&self, physical_line: usize, column: usize) -> Option<LogicalLocation> {
        if physical_line >= self.line_count {
            return None;
        }
        let index = self
            .segments
            .partition_point(|segment| segment.physical_start <= physical_line)
            - 1;
        let segment = &self.segments[index];
        let delta = u32::try_from(physical_line - segment.physical_start).ok()?;
        Some(LogicalLocation {
            source_name: segment.source_name.clone(),
            line: segment.logical_start.checked_add(delta)?,
            column,
        })
    }
}

fn parse_directive(line: &str, physical_line: usize) -> Option<LineDirective> {
    let mut rest = line.trim_start_matches([' ', '\t']);
    rest = rest.strip_prefix('#')?.trim_start_matches([' ', '\t']);
    rest = rest.strip_prefix("line")?;
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    rest = rest.trim_start_matches([' ', '\t']);
    let digit_count = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digit_count == 0 {
        return None;
    }
    let logical_line = rest[..digit_count].parse::<u32>().ok()?;
    if logical_line == 0 {
        return None;
    }
    rest = rest[digit_count..].trim_start_matches([' ', '\t']);
    let source_name = if rest.is_empty() {
        None
    } else {
        let quoted = rest.strip_prefix('"')?;
        let end = quoted.find('"')?;
        if !quoted[end + 1..].trim_matches([' ', '\t']).is_empty() {
            return None;
        }
        Some(Arc::<str>::from(&quoted[..end]))
    };
    Some(LineDirective {
        physical_line,
        logical_line,
        source_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_map_is_one_based() {
        let map = LogicalSourceMap::identity(2, Some(Arc::from("input.R")));
        assert_eq!(map.location(0, 3).unwrap().line, 1);
        assert_eq!(map.location(1, 3).unwrap().line, 2);
        assert_eq!(
            &*map.location(1, 0).unwrap().source_name.unwrap(),
            "input.R"
        );
        assert_eq!(map.location(2, 0), None);
    }

    #[test]
    fn parses_and_applies_line_directives_to_following_line() {
        let index = LineIndex::new(Arc::<str>::from(
            "x <- 1\n # line 20 \"generated.R\" \ny <- 2\n#line 7\nz <- 3",
        ));
        let map = LogicalSourceMap::parse(&index, Some(Arc::from("original.R")));
        assert_eq!(map.directives().len(), 2);
        assert_eq!(map.location(1, 0).unwrap().line, 2);
        let generated = map.location(2, 4).unwrap();
        assert_eq!(generated.line, 20);
        assert_eq!(generated.column, 4);
        assert_eq!(&*generated.source_name.unwrap(), "generated.R");
        let renamed = map.location(4, 0).unwrap();
        assert_eq!(renamed.line, 7);
        assert_eq!(&*renamed.source_name.unwrap(), "generated.R");
    }

    #[test]
    fn ignores_malformed_directives() {
        let index = LineIndex::new(Arc::<str>::from(
            "#line\n#line 0\n#line 2 trailing\n# line 3 \"unterminated",
        ));
        assert!(LogicalSourceMap::parse(&index, None)
            .directives()
            .is_empty());
    }

    #[test]
    fn validates_manual_directives() {
        let error = LogicalSourceMap::from_directives(
            1,
            None,
            [LineDirective {
                physical_line: 1,
                logical_line: 2,
                source_name: None,
            }],
        )
        .unwrap_err();
        assert!(matches!(
            error,
            LogicalSourceMapError::DirectiveLineOutOfBounds { .. }
        ));
    }
}
