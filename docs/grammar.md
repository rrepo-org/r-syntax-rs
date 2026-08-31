# Grammar Production Inventory

This is normative EBNF for the lossless Phase 1 CST, not a copy of R's yacc
implementation. Trivia may occur between terminals unless a lexical form says
otherwise. `NL*` means trivia including newline; `SP*` excludes newline.
`expr(min)` delegates infix binding to `operators.md`. Recovery can place an
`error` child wherever a required symbol is absent.

<!-- inventory:grammar:start -->
| ID | Production |
| --- | --- |
| `G001` | `source_file := NL* expr_sequence? EOF` |
| `G002` | `expr_sequence := expr (separator expr?)*` |
| `G003` | `separator := SEMICOLON NL* | NEWLINE NL*` |
| `G004` | `expr := assignment_expr` |
| `G005` | `assignment_expr := formula_expr (assignment_op assignment_expr)?` |
| `G006` | `formula_expr := help_expr (TILDE formula_expr)? | TILDE formula_expr` |
| `G007` | `help_expr := logical_or2_expr (QUESTION logical_or2_expr)* | QUESTION logical_or2_expr` |
| `G008` | `logical_or2_expr := logical_or_expr (OR2 logical_or_expr)*` |
| `G009` | `logical_or_expr := logical_and2_expr (OR logical_and2_expr)*` |
| `G010` | `logical_and2_expr := logical_and_expr (AND2 logical_and_expr)*` |
| `G011` | `logical_and_expr := not_expr (AND not_expr)*` |
| `G012` | `not_expr := NOT not_expr | comparison_expr` |
| `G013` | `comparison_expr := additive_expr (comparison_op additive_expr)*` |
| `G014` | `additive_expr := multiplicative_expr ((PLUS | MINUS) multiplicative_expr)*` |
| `G015` | `multiplicative_expr := special_pipe_expr ((STAR | SLASH) special_pipe_expr)*` |
| `G016` | `special_pipe_expr := pipe_bind_expr ((SPECIAL | WALRUS | PIPE) pipe_bind_expr)*` |
| `G017` | `pipe_bind_expr := colon_expr (PIPE_BIND colon_expr)*` |
| `G018` | `colon_expr := unary_expr (COLON unary_expr)*` |
| `G019` | `unary_expr := (PLUS | MINUS | TILDE | QUESTION) unary_expr | power_expr` |
| `G020` | `power_expr := postfix_expr (CARET unary_expr)?` |
| `G021` | `postfix_expr := primary_expr postfix_part*` |
| `G022` | `postfix_part := call | subset | subset2 | component | namespace` |
| `G023` | `primary_expr := literal | name | PLACEHOLDER | paren_expr | block | function_expr | if_expr | for_expr | while_expr | repeat_expr | jump_expr` |
| `G024` | `literal := NUMBER | STRING | NULL | TRUE | FALSE | NA | INF | NAN | NA_INTEGER | NA_REAL | NA_COMPLEX | NA_CHARACTER` |
| `G025` | `name := IDENTIFIER | DOT_DOT_I` |
| `G026` | `paren_expr := LPAREN NL* expr NL* RPAREN` |
| `G027` | `block := LBRACE NL* expr_sequence? RBRACE` |
| `G028` | `function_expr := FUNCTION SP* LPAREN NL* formal_list? RPAREN NL* expr` |
| `G029` | `formal_list := formal (NL* COMMA NL* formal)* NL* COMMA?` |
| `G030` | `formal := (name | ELLIPSIS) (SP* ASSIGN_EQ NL* expr)?` |
| `G031` | `if_expr := IF SP* LPAREN NL* expr NL* RPAREN NL* expr (SP* ELSE NL* expr)?` |
| `G032` | `for_expr := FOR SP* LPAREN NL* name NL* IN NL* expr NL* RPAREN NL* expr` |
| `G033` | `while_expr := WHILE SP* LPAREN NL* expr NL* RPAREN NL* expr` |
| `G034` | `repeat_expr := REPEAT NL* expr` |
| `G035` | `jump_expr := NEXT | BREAK` |
| `G036` | `call := LPAREN NL* argument_list? RPAREN` |
| `G037` | `argument_list := argument? (NL* COMMA NL* argument?)*` |
| `G038` | `argument := ((name | STRING | NULL) SP* ASSIGN_EQ NL*)? expr` |
| `G039` | `subset := LBRACKET NL* subscript_list? RBRACKET` |
| `G040` | `subset2 := LDBRACKET NL* subscript_list? RBRACKET RBRACKET` |
| `G041` | `subscript_list := argument? (NL* COMMA NL* argument?)*` |
| `G042` | `component := (DOLLAR | AT) SP* (name | STRING)` |
| `G043` | `namespace := (NS_GET | NS_GET_INTERNAL) SP* (name | STRING)` |
| `G044` | `assignment_op := ASSIGN_LEFT | ASSIGN_LEFT2 | ASSIGN_EQ | ASSIGN_RIGHT | ASSIGN_RIGHT2` |
| `G045` | `comparison_op := LT | LE | GT | GE | EQ | NE` |
<!-- inventory:grammar:end -->

`ELLIPSIS` is the identifier spelling `...`, retained as `T_IDENTIFIER`; it is
written as a grammar alias to make formal-list checks explicit. Uppercase grammar
terminals map to token IDs by adding `T_`. `EOF` and recovery `MISSING` are
synthetic. The sidecar productions consume views of host comment text and never
replace host tokens.

Recovery nodes and roxygen sidecars are CST facilities orthogonal to the R
expression grammar: `error := ERROR | MISSING | unexpected-token subtree`,
`roxygen_sidecar := roxygen_block*`, and a block is a newline-separated run of
`ROXYGEN_COMMENT` tokens. Assignments require parser-time target checks matching
R syntax. Rightward assignment associates left while leftward/equal assignment
associates right; the implementation follows the precedence table rather than
the simplified optional recursion shown in `G005`.

## Roxygen sidecar productions

Tag-specific semantics are deferred, but Phase 1 closes the lossless structural
grammar. A known tag may refine `tag_head`; unknown tags use the same generic
shape. Example and fenced-code payloads are opaque code tokens, never executed.

<!-- inventory:roxygen-grammar:start -->
| ID | Production |
| --- | --- |
| `RG001` | `sidecar := roxygen_block*` |
| `RG002` | `roxygen_block := roxygen_line (RT_NEWLINE roxygen_line)*` |
| `RG003` | `roxygen_line := RT_LINE_PREFIX RT_WHITESPACE? (tag | description)?` |
| `RG004` | `tag := RT_TAG_MARK RT_TAG_NAME RT_WHITESPACE? tag_body?` |
| `RG005` | `tag_body := param_body | section_body | code_body | generic_body` |
| `RG006` | `param_body := name_list RT_WHITESPACE description?` |
| `RG007` | `name_list := RT_IDENTIFIER (RT_COMMA RT_WHITESPACE? RT_IDENTIFIER)*` |
| `RG008` | `section_body := inline* RT_COLON RT_WHITESPACE? description?` |
| `RG009` | `code_body := RT_CODE_BLOCK` |
| `RG010` | `generic_body := inline*` |
| `RG011` | `description := inline (RT_WHITESPACE inline)*` |
| `RG012` | `inline := RT_TEXT | RT_IDENTIFIER | RT_CODE_SPAN | link | punctuation` |
| `RG013` | `link := RT_LBRACKET inline* RT_RBRACKET` |
| `RG014` | `punctuation := RT_COMMA | RT_COLON | RT_LPAREN | RT_RPAREN` |
| `RG015` | `roxygen_error := RT_ERROR | RT_MISSING | unexpected-token subtree` |
<!-- inventory:roxygen-grammar:end -->
