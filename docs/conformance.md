# Conformance and Oracle Design

## Corpus layout

A corpus case consists of a portable case ID, UTF-8 source, compatibility
profile, tags, and optional expected fingerprints. A result records tree and
diagnostic fingerprints plus counts and completion status. `r-conformance`
depends on the production Rust crates to provide adapters for `ParseSnapshot`
and `RoxygenParse`; it has no dependency on R and no process execution API.

Adapters emit a canonical preorder stream:

```text
StartNode(kind), Token(kind, exact_text), ..., FinishNode
```

Kinds are stable textual names, not numeric enum discriminants. Length-framed
bytes prevent concatenation ambiguity. A tree stream must have one balanced
root. Generic diagnostic sets are sorted by canonical severity, range, code,
message, and notes. The production snapshot adapter preserves parser diagnostic
order and includes code, severity, range, message, and recovery metadata.

The current fingerprint is a deterministic 256-bit non-cryptographic digest
with an algorithm/version domain separator. It is suitable for regression IDs,
not adversarial integrity or proof of semantic equivalence. Changing canonical
encoding or hashing requires a new algorithm label and fixture regeneration.

The frozen cases under `crates/r-conformance/fixtures` cover precedence,
malformed and incomplete input, exact text retention, roxygen block grouping,
tags and fences, embedded-example mappings, and `NULL` association termination.
Their reviewed TSV fingerprints are Rust regression expectations. Tests also
exercise arbitrary UTF-8 strings, every lexer-token-boundary prefix, repeated
parsing, and concurrent parsing without a `cargo-fuzz` dependency.

## Offline oracle protocol

R 4.6.1 and roxygen2 8.1.0 are reference oracles only for a separately operated,
explicit fixture-generation workflow outside production and outside `xtask`.
Nothing in this repository automatically discovers, installs, fetches, embeds,
starts, or invokes R. Tests and checks consume reviewed, committed fixture data.

No committed fixture currently establishes executable-oracle parity. Producing
that evidence remains external work and requires an exactly provisioned R 4.6.1
environment (plus roxygen2 8.1.0 for documentation observations).

Fixture producers must:

1. Run manually or in an explicitly provisioned oracle CI job, never as a build
   script, test fallback, library call, or default task.
2. Pin exact R 4.6.1, roxygen2 8.1.0, locale, platform metadata, and generation
   script revision in provenance.
3. Capture source inputs and raw oracle outputs before translating them into
   assertions; do not treat R's internal parse-data shape as the required CST.
4. Review and commit regenerated fixtures as ordinary source changes.
5. Fail closed when fixtures are absent or stale. Never consult ambient R to
   make a failing test pass.

`OracleRecord` stores declared implementation/version, generation timestamp,
script revision, and a human-readable command description. It is data only and
has no execution method. Production parser behavior is entirely independent of
this metadata.

## Comparison levels

| Level | Compared data | Intended use |
| --- | --- | --- |
| Lexical | Exact token kinds and text coverage | Boundaries, literals, trivia |
| Structural | Canonical tree fingerprint or reviewed event stream | CST regressions |
| Diagnostic | Canonical diagnostic-set fingerprint and optional exact fields | Recovery regressions |
| Oracle semantic | Reviewed stored observations | Cases where R acceptance informs syntax |
| Roxygen sidecar | Host comment preservation plus independent sidecar fingerprint | Documentation syntax |

Every oracle disagreement is triaged: implementation defect, intentional CST
shape difference, unsupported semantic behavior, or version-specific fixture.
Oracle output is evidence, not an unchecked golden truth.
