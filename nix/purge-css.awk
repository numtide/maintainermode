# Drops the CSS rules no page can match. The first file lists the words of
# the sources, one per line. The second is CSS as `lightningcss` prints it
# without --minify: every `{` ends a line and every `}` stands on its own line.
# A selector stays when each class it names is a word of the sources. Classes
# inside :not(), :is(), :where() and :has() do not count, so the result can
# keep too much but never too little. Themes other than light and dark go.

FNR == NR {
  used[$0] = 1
  next
}

function keep(sel, s, c) {
  if (sel ~ /data-light-theme="/ && sel !~ /data-light-theme="light"/) return 0
  if (sel ~ /data-dark-theme="/ && sel !~ /data-dark-theme="dark"/) return 0
  s = sel
  gsub(/\[[^]]*\]/, "", s)
  while (gsub(/:(not|is|where|has|matches|-webkit-any|-moz-any)\([^()]*\)/, "", s)) {}
  while (match(s, /\.[A-Za-z_-][A-Za-z0-9_-]*/)) {
    c = substr(s, RSTART + 1, RLENGTH - 1)
    if (!(c in used)) return 0
    s = substr(s, RSTART + RLENGTH)
  }
  return 1
}

# The selectors of `list` that stay, joined again, or "".
function filter(list, n, i, ch, depth, part, out) {
  depth = 0
  part = ""
  out = ""
  n = length(list)
  for (i = 1; i <= n + 1; i++) {
    ch = i <= n ? substr(list, i, 1) : ","
    if (ch == "(" || ch == "[") depth++
    if (ch == ")" || ch == "]") depth--
    if (ch == "," && depth == 0) {
      sub(/^ +/, "", part)
      if (part != "" && keep(part)) out = out (out == "" ? "" : ", ") part
      part = ""
    } else {
      part = part ch
    }
  }
  return out
}

function emit(text) {
  if (top == 0) printf "%s", text
  else buf[top] = buf[top] text
}

/\{$/ {
  line = $0
  match(line, /^ */)
  indent = substr(line, 1, RLENGTH)
  head = substr(line, RLENGTH + 1)
  sub(/ *\{$/, "", head)
  top++
  buf[top] = ""
  if (head ~ /^@/) {
    kind[top] = "at"
    open[top] = line
    alive[top] = top == 1 || alive[top - 1]
    frames[top] = head ~ /^@(-webkit-)?keyframes/
  } else if (frames[top - 1] && top > 1) {
    kind[top] = "rule"
    open[top] = line
    alive[top] = alive[top - 1]
    frames[top] = 0
  } else {
    kept = filter(head)
    kind[top] = "rule"
    open[top] = indent kept " {"
    alive[top] = kept != "" && (top == 1 || alive[top - 1])
    frames[top] = 0
  }
  next
}

/^ *\}$/ {
  text = ""
  if (alive[top] && (kind[top] == "rule" || buf[top] ~ /[^ \n]/))
    text = open[top] "\n" buf[top] $0 "\n"
  top--
  emit(text)
  next
}

{
  if (top == 0 || alive[top]) emit($0 "\n")
}
