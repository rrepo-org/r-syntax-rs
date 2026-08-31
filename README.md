# r-syntax-rs

`r-syntax-rs` is a deterministic, lossless Rowan syntax stack for R source and
roxygen comments. The implemented workspace contains:

- `r-source`: immutable source text, decoding profiles, line indexes, and
  physical/logical coordinate mapping.
- `r-syntax`: stable R syntax kinds, Rowan aliases, diagnostics, immutable
  `ParseSnapshot`s, validation, fingerprints, and typed AST wrappers.
- `r-lexer`: stateless lossless lexing with token metadata and resource limits.
- `r-parser`: error-tolerant source/expression/interactive entry points that
  preserve trivia and malformed input in a CST.
- `r-roxygen`: independent lossless sidecar trees, block association,
  host/projected mappings, tag registries, and optional syntax-only parsing of
  example bodies.
- `r-conformance`: canonical production adapters, frozen process-free fixtures,
  stable regression fingerprints, and property-style harness functions.
- `xtask`: repository inventory and consistency checks.

The default compatibility targets are exactly R 4.6.1, roxygen2 8.1.0, and
Rowan 0.17. These are pinned data profiles. No crate, test, build script, or
`xtask` command discovers or invokes R.

## Commands

```text
cargo test --workspace
cargo run -p xtask -- inventory
cargo run -p xtask -- check
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

The `xtask` commands inspect committed repository files only. The conformance
fixtures exercise this Rust implementation; they are not evidence of oracle
parity. Exact comparison with R still requires a separately provisioned R
4.6.1 environment and reviewed, externally generated observations.

## Contracts

- [`docs/phase-1.md`](docs/phase-1.md): current phase status and non-goals
- [`docs/tokens.md`](docs/tokens.md): host and roxygen token inventories
- [`docs/grammar.md`](docs/grammar.md): concrete host and roxygen productions
- [`docs/operators.md`](docs/operators.md): precedence and associativity contract
- [`docs/roxygen.md`](docs/roxygen.md): sidecar architecture and public contract
- [`docs/conformance.md`](docs/conformance.md): canonical fixtures and oracle boundary
