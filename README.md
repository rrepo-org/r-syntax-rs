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
- `r-corpus`: durable rrepo acquisition, content-addressed source storage,
  isolated parser workers, differential gates, and reproducible reports.
- `xtask`: repository checks and explicit corpus workflows.

The default compatibility targets are exactly R 4.6.1, roxygen2 8.1.0, and
Rowan 0.17. These are pinned data profiles. The parser, production crates,
build scripts, and default tests neither discover nor invoke R. The only
executable R path is the explicit opt-in corpus oracle: it starts a
digest-pinned, network-disabled container and never invokes host R.

## Commands

```text
cargo test --workspace
cargo run -p xtask -- inventory
cargo run -p xtask -- check
cargo xtask corpus snapshot --help
cargo xtask corpus collect --help
cargo xtask corpus run --help
cargo xtask corpus oracle --help
cargo xtask corpus diff --help
cargo xtask corpus replay --help
cargo xtask corpus minimize --help
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

`inventory` and `check` inspect committed repository files only. The `corpus`
subcommands are explicit integration operations; only snapshot/collection use
the network, and only `corpus oracle` starts a container. See
[`docs/corpus.md`](docs/corpus.md) for inputs, artifacts, safety rules, caching,
and gate policy. Default `cargo test --workspace` remains network-, container-,
and R-free.

## Contracts

- [`docs/phase-1.md`](docs/phase-1.md): current phase status and non-goals
- [`docs/tokens.md`](docs/tokens.md): host and roxygen token inventories
- [`docs/grammar.md`](docs/grammar.md): concrete host and roxygen productions
- [`docs/operators.md`](docs/operators.md): precedence and associativity contract
- [`docs/roxygen.md`](docs/roxygen.md): sidecar architecture and public contract
- [`docs/conformance.md`](docs/conformance.md): canonical fixtures and oracle boundary
- [`docs/corpus.md`](docs/corpus.md): acquisition and differential corpus operations
