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

## Corpus oracle protocol

R 4.6.1 and roxygen2 8.1.0 are reference profiles, not production dependencies.
The parser, production crates, build scripts, and default tests do not discover,
install, start, or invoke R. Tests and checks consume reviewed fixture data and
remain network-, container-, and R-free.

Executable comparison is an explicit `cargo xtask corpus oracle` operation. It
accepts only Docker or Podman and an OCI image named by a full `@sha256:` digest;
it never invokes host R. The container has no network, a read-only root and
read-only corpus/script mounts, dropped capabilities, no-new-privileges, bounded
CPU, memory, and PIDs, and a small `noexec` temporary filesystem. The checked-in
script calls `parse(..., keep.source = TRUE)` but does not source, evaluate,
load, install, or otherwise execute corpus code.

Oracle producers must:

1. Run manually or in explicitly provisioned oracle CI, never as a build script,
   test fallback, production parser call, or default task.
2. Pin exact R 4.6.1, roxygen2 8.1.0, image digest, locale, platform, and oracle
   script digest in provenance.
3. Capture source inputs and raw oracle outputs before translating them into
   assertions; do not treat R's internal parse-data shape as the required CST.
4. Match each result to the decoded-source SHA-256 and fail closed on missing,
   duplicate, extra, or mismatched records.
5. Review promoted observations as ordinary source changes; never consult
   ambient R to make a failing test pass.

The process-free `r-conformance::OracleRecord` remains data only. The corpus
tool's separate oracle ledger records image, R version, locale, platform, and
the hash of the exact embedded script mounted into the container. Production
parser behavior is entirely independent of both forms of metadata.

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

## Differential gates

Candidate crashes, signals, invariant violations, resource-limit failures,
missing candidate cases, source mismatches, and R-accepts/candidate-rejects are
hard failures. Candidate/R acceptance differences otherwise require review;
R-rejects/candidate-accepts is hard only for curated cases. Acquisition,
decoding, archive, oracle-infrastructure, and thresholded performance findings
remain visible and are never dropped by an inner join. Stable, path-independent
signatures cluster equivalent findings. Full corpus accounting, replay bundles,
and minimization are specified in [`corpus.md`](corpus.md).
