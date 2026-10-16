{ self, pkgs }:
pkgs.testers.runNixOSTest {
  name = "maintainer-mode";
  nodes.machine =
    { config, ... }:
    {
      imports = [ self.nixosModules.default ];
      services.maintainer-mode.enable = true;
      services.maintainer-mode.githubAppId = 1;
      # a throwaway key, made for the test only
      services.maintainer-mode.githubAppKeyFile = pkgs.runCommand "test-github-app-key" {
        nativeBuildInputs = [ pkgs.openssl ];
      } "openssl genrsa -traditional -out $out 2048";
      virtualisation.memorySize = 1024;
      environment.systemPackages = [ config.services.maintainer-mode.package ];
      # a tree with one private package and a stand-in nixpkgs that knows
      # the other two, so the client scans with no network
      environment.etc."scan/host.nix".text = ''
        let
          mk = pname: version: deps: derivation {
            name = "''${pname}-''${version}";
            inherit pname version deps;
            system = builtins.currentSystem;
            builder = "/bin/sh";
          };
          private = mk "our-private-daemon" "1.0" [ (mk "hello" "2.12" [ ]) (mk "requests" "2.32" [ ]) ];
        in
        { top = mk "system" "1" [ private ]; }
      '';
      environment.etc."scan/nixpkgs/default.nix".text = ''
        { system ? builtins.currentSystem, ... }:
        let
          mk = pname: version: derivation {
            name = "''${pname}-''${version}";
            inherit pname version system;
            builder = "/bin/sh";
          };
        in
        { hello = mk "hello" "2.12"; python3Packages.requests = mk "requests" "2.32"; }
      '';
    };
  # no network in the test: the sync and the sweep fail and retry, and the
  # pages must still answer
  testScript = ''
    machine.wait_for_unit("maintainer-mode.service")
    machine.wait_for_open_port(8790)
    machine.succeed("curl -sf http://127.0.0.1:8790/healthz")
    machine.succeed("curl -sf http://127.0.0.1:8790/ -o /tmp/page && grep -q 'Maintainer Mode' /tmp/page")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1/status -o /tmp/page && grep -q move_days /tmp/page")
    machine.succeed("curl -sf http://127.0.0.1:8790/static/primer.css >/dev/null")
    machine.succeed("curl -sf http://127.0.0.1:8790/rules -o /tmp/rules && grep -q 'id=\"the-rules\"' /tmp/rules")
    machine.succeed("curl -sf http://127.0.0.1:8790/state | grep -c 'State of nixpkgs'")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1/metrics | grep -c 'concentration_365d'")
    machine.fail("curl -sf http://127.0.0.1:8790/api/v1/people")
    machine.succeed("curl -sf http://127.0.0.1:8790/packages -o /tmp/page && grep -q 'Packages, modules and areas' /tmp/page")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/packages?q=hello' -o /tmp/page && grep -q 'Nothing matches' /tmp/page")
    machine.succeed("curl -s http://127.0.0.1:8790/p/hello -o /tmp/page && grep -q 'first index is still being built' /tmp/page")
    machine.fail("curl -sf http://127.0.0.1:8790/p/hello")
    machine.succeed("curl -sf http://127.0.0.1:8790/a/pkgs/by-name -o /tmp/area && grep -q 'pkgs/by-name/' /tmp/area")
    machine.succeed("curl -sf http://127.0.0.1:8790/u/nobody/maintains -o /tmp/page && grep -q 'What nobody maintains' /tmp/page")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1/maintainers/nobody -o /tmp/page && grep -q '\"packages\":\\[\\]' /tmp/page")
    machine.succeed("curl -sf http://127.0.0.1:8790/watch -o /tmp/page && grep -q 'Sign in to subscribe' /tmp/page")
    machine.fail("curl -sf http://127.0.0.1:8790/feed/nothing")
    machine.succeed("curl -sf http://127.0.0.1:8790/actions -o /tmp/page && grep -q 'Sign in to run workflows' /tmp/page")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -d pr=1 http://127.0.0.1:8790/actions/review | grep -q 303")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -H 'Sec-Fetch-Site: same-site' -d pr=1 http://127.0.0.1:8790/actions/review | grep -q 403")
    machine.fail("curl -sf http://127.0.0.1:8790/a/..%2Fetc")
    # the event stream never ends, so curl stops at its time limit (exit 28);
    # curl creates the body file only when data arrives
    machine.succeed("touch /tmp/events")
    status = machine.execute("curl -s -m 3 -D /tmp/events.h -o /tmp/events -H 'Last-Event-ID: 0' 'http://127.0.0.1:8790/api/v1/events?type=PushEvent'")[0]
    assert status == 28, f"curl on the event stream exits {status}, not 28"
    machine.succeed("grep -qi 'content-type: text/event-stream' /tmp/events.h")
    machine.succeed("! grep -q '^data:' /tmp/events")
    machine.succeed("test -d /var/lib/maintainer-mode/events")
    status = machine.execute("maintainer-mode --server http://127.0.0.1:8790 check nobody")[0]
    assert status == 3, f"check exits {status}, not 3"
    # a reader that closes stdout early must not make the CLI panic
    machine.succeed("set -o pipefail; maintainer-mode --server http://127.0.0.1:8790 metrics | true")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST http://127.0.0.1:8790/api/v1/reports > /tmp/r1 && grep -q 401 /tmp/r1")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -H 'Authorization: Bearer nope' -d '{}' http://127.0.0.1:8790/api/v1/reports > /tmp/r2 && grep -q 401 /tmp/r2")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8790/me/sets > /tmp/r3 && grep -q 303 /tmp/r3")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST http://127.0.0.1:8790/me/upload-token > /tmp/r4 && grep -q 303 /tmp/r4")
    machine.succeed("cp -rL /etc/scan /tmp/scan")
    machine.succeed("cd /tmp/scan && maintainer-mode scan --file host.nix --attr top --nixpkgs ./nixpkgs")
    machine.succeed("grep -q our-private-daemon /tmp/scan/maintainer-mode-scan.json")
    machine.succeed("cd /tmp/scan && maintainer-mode export --out report.json")
    machine.succeed("grep -q '\"attr\": \"python3Packages.requests\"' /tmp/scan/report.json")
    machine.succeed("grep -q '\"unknown\": 2' /tmp/scan/report.json")
    machine.fail("grep -q -e our-private-daemon -e host.nix /tmp/scan/report.json")
    machine.fail("cd /tmp/scan && maintainer-mode submit --to http://127.0.0.1:8790")
    machine.fail("cd /tmp/scan && MAINTAINER_MODE_UPLOAD_TOKEN=nope maintainer-mode submit --to http://127.0.0.1:8790")
    machine.succeed("test -f /var/lib/maintainer-mode/maintainer-mode.sqlite")
    machine.wait_until_succeeds("journalctl -u maintainer-mode -o cat > /tmp/log && grep -q 'sync token from GitHub App 1' /tmp/log")
    # the key parsed and the JWT was signed; only the request found no network
    machine.wait_until_succeeds("journalctl -u maintainer-mode -o cat > /tmp/log && grep -q 'GitHub App token: error sending request' /tmp/log")
    machine.wait_until_succeeds("journalctl -u maintainer-mode -o cat > /tmp/log && grep -q 'next pass in 10 s' /tmp/log")
  '';
}
