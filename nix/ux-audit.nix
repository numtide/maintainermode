{
  lib,
  writeShellApplication,
  makeFontsConf,
  dejavu_fonts,
  liberation_ttf,
  nodejs,
  playwright-test,
  playwright-driver,
  sqlite,
  # the server that --local starts; null leaves --bin or target/debug
  maintainer-mode ? null,
}:
writeShellApplication {
  name = "ux-audit";
  runtimeInputs = [ sqlite ];
  text = ''
    export NODE_PATH=${playwright-test}/lib/node_modules
    export PLAYWRIGHT_BROWSERS_PATH=${playwright-driver.browsers}
    export PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS=true
    export FONTCONFIG_FILE=${
      makeFontsConf {
        fontDirectories = [
          dejavu_fonts
          liberation_ttf
        ];
      }
    }
    export UX_AUDIT_PAGES=${./ux-pages.txt}
    ${lib.optionalString (maintainer-mode != null) ''
      export MAINTAINER_MODE_BIN="''${MAINTAINER_MODE_BIN:-${lib.getExe maintainer-mode}}"
    ''}
    exec ${lib.getExe nodejs} ${./ux-audit.js} "$@"
  '';
}
