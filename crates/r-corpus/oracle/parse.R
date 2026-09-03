args <- commandArgs(trailingOnly = TRUE)
if (length(args) != 1L) stop("usage: parse.R CORPUS_ROOT")

expected_version <- Sys.getenv("EXPECTED_R_VERSION", unset = NA_character_)
actual_version <- paste(R.version$major, R.version$minor, sep = ".")
if (is.na(expected_version) || !identical(actual_version, expected_version)) {
  stop(sprintf("expected R %s, found R %s", expected_version, actual_version))
}
expected_locale <- Sys.getenv("LC_ALL", unset = NA_character_)
actual_locale <- Sys.getlocale("LC_CTYPE")
if (is.na(expected_locale) || !identical(actual_locale, expected_locale)) {
  stop(sprintf("expected locale %s, found locale %s", expected_locale, actual_locale))
}

json_string <- function(value) {
  pieces <- vapply(utf8ToInt(enc2utf8(value)), function(point) {
    if (point == 34L) return('\\"')
    if (point == 92L) return('\\\\')
    if (point < 32L) return(sprintf("\\u%04x", point))
    intToUtf8(point)
  }, character(1L), USE.NAMES = FALSE)
  paste0('"', paste0(pieces, collapse = ""), '"')
}

root <- normalizePath(args[[1L]], winslash = "/", mustWork = TRUE)
paths <- sort(list.files(root, recursive = TRUE, full.names = TRUE, all.files = FALSE))
for (path in paths) {
  relative <- substring(normalizePath(path, winslash = "/", mustWork = TRUE), nchar(root) + 2L)
  result <- tryCatch({
    parse(file = path, keep.source = TRUE)
    '{"status":"accepted"}'
  }, error = function(condition) {
    call <- conditionCall(condition)
    line <- if (!is.null(call) && !is.null(attr(call, "srcref"))) attr(call, "srcref")[[1L]] else NA_integer_
    line_json <- if (is.na(line)) "null" else as.character(line)
    paste0('{"status":"rejected","message":', json_string(conditionMessage(condition)),
           ',"line":', line_json, ',"column":null}')
  })
  cat('{"path":', json_string(relative), ',"outcome":', result, '}\n', sep = "")
}
