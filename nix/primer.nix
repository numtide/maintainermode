# Primer CSS and the Octicons the pages use, from the npm registry tarballs.
# Both MIT; the licence files are kept next to the assets.
{ fetchurl, runCommand }:
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
    "law"
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
    "key"
    "versions"
    "flame"
  ];
in
runCommand "primer-assets" { } ''
  mkdir -p css oct $out/octicons
  tar xzf ${css} -C css
  tar xzf ${octicons} -C oct
  cp css/package/dist/primer.css $out/primer.css
  cp css/package/LICENSE $out/LICENSE.primer-css
  cp oct/package/LICENSE $out/LICENSE.octicons
  for i in ${toString icons}; do
    cp oct/package/build/svg/$i-16.svg $out/octicons/
  done
''
