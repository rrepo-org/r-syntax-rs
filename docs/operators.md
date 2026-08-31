# Operators and Contexts

Rows are ordered from lowest to highest binding power, matching the R 4.6.1
expression grammar. Operators on one row share precedence. Prefix forms are
called out separately where a spelling is overloaded.

<!-- inventory:operators:start -->
| ID | Tokens | Associativity | Arity | Context and constraints |
| --- | --- | --- | --- | --- |
| `O01` | `T_QUESTION` | left | binary | Help expression; also prefix unary. |
| `O02` | `T_ASSIGN_LEFT`, `T_ASSIGN_LEFT2` | right | binary | Leftward assignment and target checks. |
| `O03` | `T_ASSIGN_EQ` | right | binary | Assignment; tags formals/arguments only in their list contexts. |
| `O04` | `T_ASSIGN_RIGHT`, `T_ASSIGN_RIGHT2` | left | binary | Rightward assignment. |
| `O05` | `T_TILDE` | right | binary | Formula; also prefix unary. |
| `O06` | `T_OR2` | left | binary | Short-circuit logical OR syntax. |
| `O07` | `T_OR` | left | binary | Vector logical OR syntax. |
| `O08` | `T_AND2` | left | binary | Short-circuit logical AND syntax. |
| `O09` | `T_AND` | left | binary | Vector logical AND syntax. |
| `O10` | `T_NOT` | right | prefix | Unary logical negation; its R precedence is below comparisons. |
| `O11` | `T_LT`, `T_LE`, `T_GT`, `T_GE`, `T_EQ`, `T_NE` | left | binary | Comparison chains are syntactically retained. |
| `O12` | `T_PLUS`, `T_MINUS` | left | binary | Additive; both spellings also have prefix forms. |
| `O13` | `T_STAR`, `T_SLASH` | left | binary | Multiplicative. |
| `O14` | `T_SPECIAL`, `T_WALRUS`, `T_PIPE` | left | binary | User infix/native pipe level; native-pipe checks P009-P010 apply. |
| `O15` | `T_PIPE_BIND` | left | binary | Special native pipe-bind context; P011 applies. |
| `O16` | `T_COLON` | left | binary | Sequence operator. |
| `O17` | `T_PLUS`, `T_MINUS` | right | prefix | Unary sign; exponentiation binds more tightly. |
| `O18` | `T_CARET` | right | binary | Exponentiation. |
| `O19` | `T_DOLLAR`, `T_AT` | left | binary/postfix | Member operand restrictions; P008 applies. |
| `O20` | `T_NS_GET`, `T_NS_GET_INTERNAL` | left | binary/postfix | Name-like operands; P007 applies. |
| `O21` | `T_LPAREN`, `T_LBRACKET`, `T_LDBRACKET` | left | postfix | Call/subset; highest binding postfix forms. |
<!-- inventory:operators:end -->

## Context table

| Context | Newline behavior | `=` interpretation | Empty item | Placeholder |
| --- | --- | --- | --- | --- |
| Top-level/braced sequence | Terminates a complete expression unless continuation is required | Assignment operator | Not an expression | Invalid |
| Parenthesized expression | Trivia while expression is incomplete | Assignment operator | Invalid | Pipe rules only |
| Call arguments | Trivia around separators | Top-level name/string/`NULL` tag; nested `=` is assignment | Missing argument represented explicitly | Named argument value on pipe RHS only |
| Formal parameters | Trivia around separators | Default separator | Invalid | Invalid |
| `[` subscripts | Trivia around separators | Argument tag | Missing subscript represented explicitly | Pipe rules only |
| `[[` subscript | Trivia around separators | Argument tag | Invalid in Phase 1 | Pipe rules only |
| `if` body before `else` | Newline after an unbraced complete body prevents attachment | Ordinary expression rules | Invalid | Pipe rules only |
| Native-pipe RHS | Ordinary continuation rules | Named argument tag in calls | Per call rules | At most once under P010 |
| Roxygen comment run | Host newline separates comment tokens | Roxygen sidecar grammar, not R assignment | Sidecar-defined | No R placeholder meaning |

Lexical longest-match is resolved before precedence. In particular `|>` precedes
`|`, `:::` precedes `::` and `:`, `[[` precedes `[`, and assignment variants
precede their prefixes. Trivia is attached losslessly but does not become an
operator operand.
