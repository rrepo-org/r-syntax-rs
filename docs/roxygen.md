# Roxygen Sidecar Architecture

## Purpose and boundary

`r-roxygen` projects host `ROXYGEN_COMMENT` tokens into a separate Rowan tree.
It is syntax infrastructure, not roxygen2 execution: it does not load packages,
run roclets, render Rd, evaluate examples, or invoke R. Building a sidecar does
not mutate the host `ParseSnapshot` or replace host tokens.

The default registry is the tag vocabulary labeled roxygen2 8.1.0. That label
selects deterministic syntax behavior; it does not claim output parity with a
roxygen2 installation. `TagRegistry::extended` returns a new registry so custom
tag behavior remains explicit input.

## Construction

- `parse_source` parses decoded R text and then builds its sidecar.
- `parse` builds a sidecar from an existing host snapshot with defaults.
- `parse_sidecar` accepts an existing snapshot and explicit `RoxygenConfig`.
- `RoxygenConfig::parse_examples` optionally parses recognized code-tag bodies
  with `r-parser`; this is parsing only and never evaluates code.

A block is a maximal run of `ROXYGEN_COMMENT` tokens separated only by one host
newline and optional indentation before the next roxygen comment. Other tokens
end the run. Prefixes (`#'`) and the newline between lines are retained in the
projected text; indentation outside the host comment token is not projected.
Each block records host range, projected range, and line count.

## Trees and diagnostics

The sidecar root is `SIDECAR`, containing one `ROXYGEN_BLOCK` per discovered
run. Lines preserve exact projected token text. Known tags select parameter,
section, code, or generic body grammar. Unknown tags use `OPAQUE_BODY` so their
text survives without guessing semantics. Backtick spans, links, fenced code,
and code-tag continuation lines have dedicated syntax behavior.

Roxygen diagnostics use `R-ROXYGEN-*` codes and host UTF-8 byte ranges. Missing
tokens are zero-width sidecar tokens. Diagnostics and recovery never alter the
host CST. `RoxygenStatus` separately reports resource limitation, incomplete
host-token discovery, and diagnostic truncation.

## Coordinates

Every `ProjectedToken` carries both a projected range and its original host
range. `host_point`/`projected_point` require `BoundaryAffinity` because a point
on a token boundary can belong to the left or right token. Range helpers use
right affinity at the start and left affinity at the end by default. Mapping can
return `None` when a point is outside projected token coverage.

When example parsing is enabled, `EmbeddedRParse` owns an immutable R
`ParseSnapshot` plus `EmbeddedMapping` entries connecting embedded, projected,
and host ranges. These mappings describe only text included in that embedded
parse; they do not imply source-map coverage outside it.

## Association

Each block receives one `BlockAssociation`:

| State | Meaning |
| --- | --- |
| `Normal` | The next top-level expression is the candidate target. |
| `Detached` | No following top-level expression exists. |
| `TerminatedByNull` | The candidate expression consists of `NULL`. |
| `Ambiguous` | Another roxygen block occurs before a candidate expression. |

`expression_range` is absent for detached or unresolved associations.
`intervening_host_range` records the bytes between the block and candidate (or
end of source). Association is a syntactic adjacency result, not roxygen2 object
collation or documentation inheritance.

## Public stability

The public contract is lossless projected text, stable kind names/values,
immutable outputs, explicit ranges and statuses, and deterministic results for
the same source/configuration. Tree shape and canonical conformance encoding are
regression contracts within their documented version labels. They are not a
claim that roxygen2 produces an equivalent internal tree.
