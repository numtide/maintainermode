# Primer CSS and the Octicons the pages use, from the npm registry tarballs.
# Both MIT; the licence files are kept next to the assets.
#
# With `classes`, a directory of sources, primer.css keeps only the rules
# whose classes appear in it, and only the light and dark themes. Without
# it, primer.css is the full file.
{
  lib,
  fetchurl,
  runCommand,
  lightningcss,
  classes ? null,
}:
let
  css = fetchurl {
    url = "https://registry.npmjs.org/@primer/css/-/css-22.3.2.tgz";
    hash = "sha512-Vjx7U/ILnGsEQP+cZ4p8blVTFcjE2TmwxTICvv5RjoBdS7QacDVllpJo+A9raJB0TxvetPiOqSR7NyHX3hcMKQ==";
  };
  octicons = fetchurl {
    url = "https://registry.npmjs.org/@primer/octicons/-/octicons-19.40.0.tgz";
    hash = "sha512-IHzIw98WUf+APnzjryvS1MT9MDlqmHqn5JG1Mi/SlrGScXofERS3lS/hatVgReCoDYvHbeh2E5S/1MGWldmtoQ==";
  };
  icons = [
    "check-circle-fill"
    "dot-fill"
    "bell"
    "eye"
    "graph"
    "git-merge"
    "git-pull-request"
    "people"
    "person"
    "sign-in"
    "sign-out"
    "tag"
    "x-circle-fill"
    "package"
    "issue-opened"
    "issue-closed"
    "search"
    "file-directory"
    "gear"
    "shield"
    "rss"
    "git-pull-request-closed"
    "skip"
    "play"
    "home"
    "key"
    "pulse"
    "circle"
    "inbox"
    "mail"
    "mark-github"
    "file-diff"
    "comment"
    "device-desktop"
    "tools"
    "upload"
    "checklist"
    "arrow-right"
    "info"
  ];
  trim = ''
    find ${classes} -type f -exec cat {} + | tr -cs 'A-Za-z0-9_-' '\n' | sort -u > words
    lightningcss --targets 'chrome >= 90, firefox >= 90, safari >= 14' css/package/dist/primer.css -o pretty.css
    awk -f ${./purge-css.awk} words pretty.css > purged.css
    lightningcss --minify --targets 'chrome >= 90, firefox >= 90, safari >= 14' purged.css -o $out/primer.css
  '';
in
runCommand "primer-assets" { nativeBuildInputs = lib.optional (classes != null) lightningcss; } ''
  mkdir -p css oct $out/octicons
  tar xzf ${css} -C css
  tar xzf ${octicons} -C oct
  ${if classes == null then "cp css/package/dist/primer.css $out/primer.css" else trim}
  cp css/package/LICENSE $out/LICENSE.primer-css
  cp oct/package/LICENSE $out/LICENSE.octicons
  for i in ${toString icons}; do
    cp oct/package/build/svg/$i-16.svg $out/octicons/
    printf '%s\t%s\n' $i "$(sed -e 's|^<svg[^>]*>||' -e 's|</svg>$||' oct/package/build/svg/$i-16.svg)" >> $out/icons.tsv
  done
''
