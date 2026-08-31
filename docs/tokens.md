# Token Inventory

The inventory is closed for Phase 1. Tokens are listed independently of Rowan
numeric discriminants. `T_MISSING` is synthetic and has empty text; every other
token covers source text. Keywords are recognized only from unquoted names.

<!-- inventory:tokens:start -->
| ID | Class | R spelling or lexical form |
| --- | --- | --- |
| `T_EOF` | sentinel | End of decoded input |
| `T_MISSING` | synthetic | Parser-inserted zero-width token |
| `T_ERROR` | recovery | Invalid or unterminated lexical fragment |
| `T_WHITESPACE` | trivia | Horizontal/other non-newline whitespace |
| `T_NEWLINE` | trivia | `\n`, `\r`, or `\r\n` as one token |
| `T_COMMENT` | trivia | `#` through before newline |
| `T_ROXYGEN_COMMENT` | trivia | `#'` through before newline |
| `T_IDENTIFIER` | atom | Syntactic or non-syntactic/backtick name |
| `T_DOT_DOT_I` | atom | `..` followed by a positive decimal index |
| `T_PLACEHOLDER` | atom | `_` in the native-pipe lexical profile |
| `T_NUMBER` | atom | R integer, double, hexadecimal, or complex literal |
| `T_STRING` | atom | Single-quoted, double-quoted, or raw string literal |
| `T_NULL` | literal | `NULL` |
| `T_TRUE` | literal | `TRUE` |
| `T_FALSE` | literal | `FALSE` |
| `T_NA` | literal | `NA` |
| `T_INF` | literal | `Inf` |
| `T_NAN` | literal | `NaN` |
| `T_NA_INTEGER` | literal | `NA_integer_` |
| `T_NA_REAL` | literal | `NA_real_` |
| `T_NA_COMPLEX` | literal | `NA_complex_` |
| `T_NA_CHARACTER` | literal | `NA_character_` |
| `T_FUNCTION` | keyword | `function` or `\\` shorthand where enabled by R 4.6.1 |
| `T_IF` | keyword | `if` |
| `T_ELSE` | keyword | `else` |
| `T_FOR` | keyword | `for` |
| `T_IN` | keyword | `in` |
| `T_WHILE` | keyword | `while` |
| `T_REPEAT` | keyword | `repeat` |
| `T_NEXT` | keyword | `next` |
| `T_BREAK` | keyword | `break` |
| `T_LPAREN` | delimiter | `(` |
| `T_RPAREN` | delimiter | `)` |
| `T_LBRACE` | delimiter | `{` |
| `T_RBRACE` | delimiter | `}` |
| `T_LBRACKET` | delimiter | `[` |
| `T_RBRACKET` | delimiter | `]` |
| `T_LDBRACKET` | delimiter | `[[` |
| `T_COMMA` | separator | `,` |
| `T_SEMICOLON` | separator | `;` |
| `T_PLUS` | operator | `+` |
| `T_MINUS` | operator | `-` |
| `T_STAR` | operator | `*` |
| `T_SLASH` | operator | `/` |
| `T_CARET` | operator | `^` |
| `T_COLON` | operator | `:` |
| `T_TILDE` | operator | `~` |
| `T_QUESTION` | operator | `?` |
| `T_NOT` | operator | `!` |
| `T_AND` | operator | `&` |
| `T_AND2` | operator | `&&` |
| `T_OR` | operator | `|` |
| `T_OR2` | operator | `||` |
| `T_LT` | operator | `<` |
| `T_LE` | operator | `<=` |
| `T_GT` | operator | `>` |
| `T_GE` | operator | `>=` |
| `T_EQ` | operator | `==` |
| `T_NE` | operator | `!=` |
| `T_ASSIGN_LEFT` | operator | `<-` |
| `T_ASSIGN_LEFT2` | operator | `<<-` |
| `T_ASSIGN_EQ` | operator | `=` |
| `T_ASSIGN_RIGHT` | operator | `->` |
| `T_ASSIGN_RIGHT2` | operator | `->>` |
| `T_WALRUS` | operator | `:=` (parsed as an operator; semantics are external) |
| `T_DOLLAR` | operator | `$` |
| `T_AT` | operator | `@` |
| `T_NS_GET` | operator | `::` |
| `T_NS_GET_INTERNAL` | operator | `:::` |
| `T_SPECIAL` | operator | Complete `%...%` user infix operator |
| `T_PIPE` | operator | `|>` |
| `T_PIPE_BIND` | operator | `=>` |
<!-- inventory:tokens:end -->

Longest-match applies to operators and delimiters. A newline is always distinct
from other whitespace because it participates in expression termination and
`else` attachment. A comment excludes its terminating newline. Roxygen comment
runs may be parsed into sidecar trees, but remain ordinary lossless host tokens.

`T_IDENTIFIER` includes backtick-quoted names as one token including delimiters.
String tokens likewise retain quotes, prefixes, escapes, and raw delimiters.
Literal constants are separate kinds because the R parser distinguishes them;
their original spelling remains token text.

## Roxygen sidecar tokens

These kinds cover the decoded payload views of `T_ROXYGEN_COMMENT` runs. They do
not consume or replace host-tree text. Unknown tag bodies remain lossless text,
allowing the sidecar grammar to evolve without changing host tokenization.

<!-- inventory:roxygen-tokens:start -->
| ID | Class | Form |
| --- | --- | --- |
| `RT_LINE_PREFIX` | trivia | Host `#'` prefix projected into the sidecar |
| `RT_WHITESPACE` | trivia | Non-newline spacing in a comment payload |
| `RT_NEWLINE` | trivia | Boundary between adjacent host comment lines |
| `RT_TAG_MARK` | punctuation | `@` beginning a tag at logical line start |
| `RT_TAG_NAME` | name | ASCII tag name following `@` |
| `RT_TEXT` | content | Unstructured description text |
| `RT_IDENTIFIER` | name | Parameter, field, or topic name |
| `RT_COMMA` | punctuation | `,` in structured tag heads |
| `RT_COLON` | punctuation | `:` ending a section title |
| `RT_LPAREN` | delimiter | `(` in structured tag heads |
| `RT_RPAREN` | delimiter | `)` in structured tag heads |
| `RT_LBRACKET` | delimiter | `[` in markdown/link content |
| `RT_RBRACKET` | delimiter | `]` in markdown/link content |
| `RT_CODE_SPAN` | content | Balanced inline backtick code span |
| `RT_CODE_BLOCK` | content | Fenced block or raw examples payload |
| `RT_ERROR` | recovery | Unterminated structured fragment |
| `RT_MISSING` | synthetic | Sidecar-inserted zero-width token |
<!-- inventory:roxygen-tokens:end -->
