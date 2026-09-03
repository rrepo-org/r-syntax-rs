# Corpus Architecture

`r-corpus` is an explicit integration harness around the production parser. It
acquires immutable rrepo snapshots, preserves provenance in a content-addressed
store (CAS), parses unique decoded content in isolated workers, optionally asks
a pinned R oracle for acceptance, and emits deterministic differential reports.
It is not linked into parser behavior or the default test path.

## Commands

The repository defines the `cargo xtask` alias. Use each command's `--help` for
paths, limits, sharding, runtime, and policy options:

```text
cargo xtask corpus snapshot --repository https://upstream.rrepo.dev/cran --output inventory.json
cargo xtask corpus collect --inventory inventory.json --store /corpus --output manifest.ndjson.zst
cargo build -p r-corpus --bin r-parse-worker
cargo xtask corpus run --manifest manifest.ndjson.zst --store /corpus --worker target/debug/r-parse-worker --implementation <commit> --records run.json.zst --results results.json.zst --cache /corpus/parser-cache
cargo xtask corpus oracle --manifest manifest.ndjson.zst --store /corpus --output oracle.json.zst --image r-base@sha256:<digest>
cargo xtask corpus diff --baseline baseline.json.zst --candidate candidate.json.zst --oracle oracle.json.zst --output diff.json.zst --summary summary.md
cargo xtask corpus replay --help
cargo xtask corpus minimize --help
```

`snapshot` records rrepo metadata; `collect` acquires exactly that snapshot
and finalizes a manifest; `run` supervises Rust parser workers; `oracle` performs
the opt-in container comparison; `diff` applies gates and clusters findings;
`diff` writes machine-readable and Markdown output and can materialize
standalone cluster bundles with `--bundles`; `replay` reruns one source; and
`minimize` deterministically reduces a
source while preserving its selected finding. Network access is confined to
snapshot and collection. R is confined to `oracle` and oracle replay.

## Inventory and manifest

The canonical JSON inventory is an immutable statement of intent: repository,
snapshot time, packages, versions, archive URLs, and any upstream authoritative
SHA-256 values. It is written without replacement and its digest identifies the
input set. It does not claim that downloads succeeded or describe their bytes.

The final manifest is the immutable result ledger for that inventory. Every
package/version attempt has exactly one terminal state: collected, rejected, or
failed. It records observed archive hashes, classified failures, and every
selected source occurrence with package/version/path, declared and decoded
encoding, raw and decoded hashes, and byte counts. Canonical NDJSON is exported
as deterministic zstd with separate content and archive digests; deterministic
archive-hash sharding does not change identity.

When rrepo supplies an authoritative archive SHA-256, collection verifies it.
Some current rrepo responses omit that field; in that case the first observed
digest is pinned locally as trust on first use (TOFU), and later changes fail
integrity checks. TOFU detects mutation only after the first observation and
cannot authenticate that first download. The intended end state is for rrepo to
publish authoritative hashes for every source archive, eliminating this gap.

## Storage and safety

Archives, raw source bytes, and decoded UTF-8 bytes are SHA-256-addressed CAS
objects. Objects are written atomically, verified when reused, and deduplicated
across packages, versions, and paths; the manifest retains all occurrences, so
deduplication never erases provenance. A SQLite ledger checkpoints terminal
acquisitions, allowing collection to resume without repeating completed work.

Collection streams each archive once under compressed, entry-count, per-entry,
total-expanded, source, and decoded-size limits. It rejects absolute or
traversing paths, non-UTF-8 or duplicate paths, links, devices, special entries,
inconsistent sizes, malformed layouts, and unsupported encodings. It reads only
the top-level `DESCRIPTION` and files below the package's `R/` directory. No
archive file or package hook is executed, and archive entries are never unpacked
as executable filesystem content.

## Workers and cache

The parser runs one decoded source per fresh worker process. The supervisor uses
bounded parallelism, wall-time and optional RSS limits, bounded stdout/stderr,
and strict one-request/one-response JSON protocol validation. Each request binds
the decoded-source SHA-256, worker protocol, parser revision, and parser config;
the worker verifies identity before parsing and reports losslessness, ranges,
snapshot validity, diagnostics, status, fingerprints, timing, memory, and
roxygen sidecars. A crash, signal, timeout, memory breach, malformed response,
or invariant failure affects one case rather than the supervisor.

Collection resumes from terminal acquisition records. Valid worker results are
cacheable only under their complete request identity; oracle results are keyed
by the input identity plus image, R version, locale, platform, and script digest.
Changing source bytes, parser identity/config, protocol, or oracle provenance
therefore invalidates reuse. Minimization has its own SHA-256 predicate cache.

## Oracle boundary

The compatibility pins remain exactly R 4.6.1, roxygen2 8.1.0, and Rowan 0.17.
Production crates and default tests do not invoke R. `cargo xtask corpus oracle`
is explicit opt-in and accepts only Docker or Podman with a full digest-pinned
OCI image. It never invokes host R. The container runs without a network, with a
read-only root and inputs, dropped capabilities, no-new-privileges, bounded
resources, and a small `noexec` temporary filesystem. Its script parses files
with `keep.source = TRUE`; corpus code is never sourced, evaluated, loaded, or
installed. Oracle acceptance is evidence, not a required CST shape.

## Accounting and gates

Every run must publish counts satisfying these equations:

```text
inventory_items = collected + rejected + failed
selected_source_occurrences = sum(collected.source_count)
selected_source_occurrences = decoded_occurrences + decode_failures
decoded_occurrences = utf8_occurrences + latin1_occurrences
unique_raw_sources = count(distinct raw_sha256)
unique_decoded_sources = count(distinct decoded_sha256)
unique_raw_sources <= source_occurrences
unique_decoded_sources <= source_occurrences
worker_cases = worker_successes + worker_failures
oracle_cases = oracle_accepted + oracle_rejected + oracle_infrastructure
findings = hard_failures + review_items
```

Parsing and oracle comparison operate on unique decoded content; occurrence
provenance maps every result back to all package/version/archive paths. Reports
must show both occurrence and unique counts, plus logical bytes, unique CAS
bytes, cache hits, and deduplication savings, rather than presenting deduplicated
content as lost input. Missing, extra, duplicate, or hash-mismatched cases are
findings, never silently excluded.

Hard gates cover candidate crashes/signals, invariant or resource failures,
missing candidate output, source mismatch, and R acceptance where the candidate
rejects. Curated silent acceptance against R is also hard; non-curated
acceptance differences and configurable performance/memory regressions are
review items by default. Acquisition, archive, decoding, and oracle
infrastructure failures remain separately classified and accounted.

## Reports, replay, and minimization

Outputs are deterministic compressed JSON/NDJSON plus a Markdown summary with
stable finding signatures and occurrence clusters. A replay bundle contains the
exact source, finding metadata, baseline/candidate/oracle records, occurrence
provenance, a byte-exact `reproducer.R`, and a hardened `replay.sh` requiring a
digest-pinned container image. Replay does not use host R or a network.

Minimization applies stable line, token, then Unicode-character reductions and
keeps only candidates that reproduce the selected predicate. It never creates
invalid UTF-8, records original/minimized hashes and sizes, and reuses cached
predicate outcomes. A minimized example supplements, rather than replaces, the
original source and provenance.

Use `--status` for a single-worker parser outcome, or `--finding-code` with
`--baseline-worker` and `--baseline` to preserve an exact differential finding
such as `candidate_crash`, `acceptance_changed`, or `tree_changed`.

Default `cargo test --workspace`, builds, repository `inventory`, and `check`
remain network-, container-, and R-free. Corpus commands are never an implicit
fallback for a failing unit or conformance test.
