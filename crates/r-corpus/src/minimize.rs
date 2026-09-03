//! Deterministic, property-preserving reduction of UTF-8 source text.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;

/// Reduction passes, applied from coarse to fine.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReductionPass {
    Lines,
    Tokens,
    Characters,
}

/// Configuration for deterministic delta reduction.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MinimizeConfig {
    pub passes: Vec<ReductionPass>,
}

impl Default for MinimizeConfig {
    fn default() -> Self {
        Self {
            passes: vec![
                ReductionPass::Lines,
                ReductionPass::Tokens,
                ReductionPass::Characters,
            ],
        }
    }
}

/// SHA-256 keyed predicate results, reusable across minimization runs.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PredicateCache {
    entries: BTreeMap<String, bool>,
}

impl PredicateCache {
    pub fn get(&self, source: &str) -> Option<bool> {
        self.entries.get(&sha256(source.as_bytes())).copied()
    }

    pub fn insert(&mut self, source: &str, value: bool) {
        self.entries.insert(sha256(source.as_bytes()), value);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MinimizationResult {
    pub source: String,
    pub original_bytes: usize,
    pub minimized_bytes: usize,
    pub predicate_calls: usize,
    pub cache_hits: usize,
    pub sha256: String,
}

#[derive(Debug)]
pub enum MinimizeError<E> {
    InitialInputDoesNotMatch,
    Predicate(E),
}

impl<E: fmt::Display> fmt::Display for MinimizeError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InitialInputDoesNotMatch => {
                f.write_str("initial input does not satisfy predicate")
            }
            Self::Predicate(error) => write!(f, "predicate failed: {error}"),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for MinimizeError<E> {}

/// Minimize `input`, retaining only candidates for which `predicate` returns true.
///
/// Candidates are attempted in a stable order. All boundaries are Rust string
/// boundaries, so malformed UTF-8 can never be produced.
pub fn minimize<E, F>(
    input: &str,
    config: &MinimizeConfig,
    cache: &mut PredicateCache,
    mut predicate: F,
) -> Result<MinimizationResult, MinimizeError<E>>
where
    F: FnMut(&str) -> Result<bool, E>,
{
    let mut calls = 0;
    let mut hits = 0;
    if !check(input, cache, &mut predicate, &mut calls, &mut hits)? {
        return Err(MinimizeError::InitialInputDoesNotMatch);
    }

    let mut current = input.to_owned();
    for pass in &config.passes {
        let units = units(&current, *pass);
        current = ddmin(units, cache, &mut predicate, &mut calls, &mut hits)?;
    }

    Ok(MinimizationResult {
        original_bytes: input.len(),
        minimized_bytes: current.len(),
        sha256: sha256(current.as_bytes()),
        source: current,
        predicate_calls: calls,
        cache_hits: hits,
    })
}

fn check<E, F>(
    candidate: &str,
    cache: &mut PredicateCache,
    predicate: &mut F,
    calls: &mut usize,
    hits: &mut usize,
) -> Result<bool, MinimizeError<E>>
where
    F: FnMut(&str) -> Result<bool, E>,
{
    if let Some(result) = cache.get(candidate) {
        *hits += 1;
        return Ok(result);
    }
    *calls += 1;
    let result = predicate(candidate).map_err(MinimizeError::Predicate)?;
    cache.insert(candidate, result);
    Ok(result)
}

fn ddmin<E, F>(
    mut parts: Vec<String>,
    cache: &mut PredicateCache,
    predicate: &mut F,
    calls: &mut usize,
    hits: &mut usize,
) -> Result<String, MinimizeError<E>>
where
    F: FnMut(&str) -> Result<bool, E>,
{
    let mut granularity = 2;
    while !parts.is_empty() {
        let chunk_size = parts.len().div_ceil(granularity);
        let mut reduced = false;
        let mut start = 0;
        while start < parts.len() {
            let end = (start + chunk_size).min(parts.len());
            let candidate = parts[..start]
                .iter()
                .chain(&parts[end..])
                .map(String::as_str)
                .collect::<String>();
            if check(&candidate, cache, predicate, calls, hits)? {
                parts.drain(start..end);
                granularity = granularity.saturating_sub(1).max(2);
                reduced = true;
                break;
            }
            start = end;
        }
        if !reduced {
            if granularity >= parts.len() {
                break;
            }
            granularity = (granularity * 2).min(parts.len());
        }
    }
    Ok(parts.concat())
}

fn units(source: &str, pass: ReductionPass) -> Vec<String> {
    match pass {
        ReductionPass::Lines => source.split_inclusive('\n').map(str::to_owned).collect(),
        ReductionPass::Characters => source.chars().map(|c| c.to_string()).collect(),
        ReductionPass::Tokens => token_units(source),
    }
}

fn token_units(source: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut class = None;
    for character in source.chars() {
        let next_class = if character.is_whitespace() {
            0
        } else if character.is_alphanumeric() || character == '_' || character == '.' {
            1
        } else {
            2
        };
        if class.is_some() && class != Some(next_class) || next_class == 2 && !current.is_empty() {
            result.push(std::mem::take(&mut current));
        }
        current.push(character);
        if next_class == 2 {
            result.push(std::mem::take(&mut current));
            class = None;
        } else {
            class = Some(next_class);
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduction_is_utf8_safe_and_deterministic() {
        let source = "discard <- 1\nkeep <- \"λ\"\nmore <- 2\n";
        let property = |text: &str| Ok::<_, std::convert::Infallible>(text.contains('λ'));
        let first = minimize(
            source,
            &MinimizeConfig::default(),
            &mut PredicateCache::default(),
            property,
        )
        .unwrap();
        let second = minimize(
            source,
            &MinimizeConfig::default(),
            &mut PredicateCache::default(),
            property,
        )
        .unwrap();
        assert_eq!(first.source, second.source);
        assert_eq!(first.source, "λ");
    }

    #[test]
    fn cache_avoids_rechecking_the_initial_input() {
        let mut cache = PredicateCache::default();
        cache.insert("x", true);
        let result = minimize(
            "x",
            &MinimizeConfig { passes: vec![] },
            &mut cache,
            |_| -> Result<bool, std::convert::Infallible> { panic!("cache should be used") },
        )
        .unwrap();
        assert_eq!(result.predicate_calls, 0);
        assert_eq!(result.cache_hits, 1);
    }

    #[test]
    fn rejects_an_input_without_the_property() {
        let result = minimize(
            "x",
            &MinimizeConfig::default(),
            &mut PredicateCache::default(),
            |_| Ok::<_, std::convert::Infallible>(false),
        );
        assert!(matches!(
            result,
            Err(MinimizeError::InitialInputDoesNotMatch)
        ));
    }
}
