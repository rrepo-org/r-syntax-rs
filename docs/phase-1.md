# Phase 1 Specification

## Status and defaults

This document records the implemented Phase 1 contract. Unless a caller explicitly selects a
different future profile, all behavior targets **R 4.6.1**, **roxygen2 8.1.0**,
and **Rowan 0.17**. Versions are semantic compatibility targets, not commands to
discover ambient installations.

The parser consumes decoded UTF-8 text and produces a lossless CST. Every input
byte represented by decoded text belongs to exactly one token, including trivia
and malformed fragments. Recovery returns a tree and diagnostics rather than
discarding the tree. Rowan green nodes are the portable snapshot; red views are
recreated on the calling thread.

## Phase 1 deliverables

1. A closed token-kind inventory in `tokens.md`.
2. A closed grammar production inventory in `grammar.md`.
3. An operator and context contract in `operators.md`.
4. Parser-time checks and recovery rules below.
5. A process-free default conformance model and opt-in corpus oracle protocol in
   `conformance.md`.

All five repository deliverables are implemented. The workspace now includes
source, syntax, lexer, parser, roxygen sidecar, conformance, and `xtask` crates.
The `r-corpus` crate adds durable acquisition and differential testing without
changing parser behavior. Committed conformance fixtures and default tests
validate Rust behavior without network, containers, or R. Executable comparison
is a separate, explicit `cargo xtask corpus oracle` operation.

## Parser-time checks

These checks are syntax diagnostics. They do not require name resolution or
evaluation.

| ID | Check | Required recovery |
| --- | --- | --- |
| P001 | Delimiters `()`, `[]`, `[[ ]]`, and `{}` are balanced and correctly nested. | Insert a zero-width missing closer at the current recovery boundary; retain an unexpected closer as `T_ERROR`. |
| P002 | `else` attaches only to the nearest unmatched `if` in the same braced or top-level expression sequence. A newline may terminate an unbraced `if` body before `else`. | Keep `else` in an error node and continue the sequence. |
| P003 | `break` and `next` occur within a lexically enclosing `for`, `while`, or `repeat`. Function boundaries reset loop context. | Build the jump node and emit a diagnostic. |
| P004 | Formal parameters are identifiers, `...`, or `..N`; a default follows exactly one `=`. `...` occurs at most once and names after it require defaults. | Preserve each malformed formal as an error child of the formal list. |
| P005 | Call arguments allow at most one top-level `=` tag. An omitted argument is represented between commas, but a trailing comma is allowed only where R allows a missing argument. | Create an explicit missing/error argument without dropping separators. |
| P006 | Subset arguments preserve omitted slots; `[[` has exactly one syntactic index expression in the Phase 1 profile. | Retain excess/missing indices under an error node. |
| P007 | Namespace operators require a namespace name on the left and a name on the right. | Build the binary node and mark the invalid operand. |
| P008 | Component operators `$` and `@` require a syntactic name or string on the right. | Build the access node and mark the invalid member. |
| P009 | A native pipe RHS is a call expression or a supported extraction chain rooted at the placeholder. | Build the pipe node and diagnose the RHS. |
| P010 | `_` is reserved for native-pipe placeholder use, occurs at most once in a pipe RHS, and in a call appears only as the value of a named argument. | Tokenize as `T_PLACEHOLDER`, retain it, and diagnose its context. |
| P011 | The experimental pipe-bind operator `=>` is preserved but disabled by default; callers must explicitly enable it in `ParserConfig`. | Preserve it as `T_PIPE_BIND` and diagnose it while disabled. |
| P012 | User-defined infix operators begin and end with `%` and may not contain newline or an unescaped `%`. | Emit one `T_ERROR` spanning the unterminated fragment. |
| P013 | Numeric, quoted, backtick, raw-string, and escape lexemes satisfy the R 4.6.1 lexical forms. | Emit a specific token where a complete boundary exists, otherwise `T_ERROR`; never split away bytes merely to validate. |
| P014 | `return` is parsed as an ordinary call/name construct according to R grammar; contextual validity is not inferred by evaluation. | No special semantic diagnostic. |
| P015 | Roxygen parsing is attempted only for `T_ROXYGEN_COMMENT` runs and produces a sidecar tree; host tokens remain unchanged. | Sidecar errors never alter the host R CST. |

Diagnostics have a stable code, severity, UTF-8 byte range, message, and
recovery metadata. Canonical production fixtures currently include all of these
fields and preserve parser diagnostic order.

## Determinism

Parsing must not inspect locale, current directory, environment variables,
loaded packages, options, wall-clock time, random state, or an R installation.
Given profile plus decoded source, token/tree/diagnostic fingerprints are stable.

## Non-goals

- Evaluating R, constructing SEXPs, resolving names, loading packages, or
  reproducing runtime errors.
- Installing, discovering, embedding, or automatically invoking host R or
  roxygen2. The opt-in corpus oracle may start only a digest-pinned,
  network-disabled R container and parses source without executing it.
- Implementing roxygen roclets, package collation, Rd rendering, examples, or
  documentation execution.
- Byte decoding or encoding detection; callers supply decoded UTF-8 and retain
  any original-byte mapping they need.
- Formatting, lint policy, type inference, data-flow analysis, or refactoring.
- Making Rowan red nodes `Send` or `Sync`; only immutable green snapshots cross
  threads.
- Claiming grammar equivalence from a tree fingerprint alone. Fingerprints are
  stable regression identifiers, not cryptographic proofs.
- Claiming acceptance, diagnostic, parse-data, or documentation parity from a
  corpus run alone. Oracle observations remain versioned evidence that must be
  reviewed and interpreted according to `conformance.md`.
