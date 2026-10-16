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
    };
  # no network in the test: the sync and the sweep fail and retry, and the
  # pages must still answer
  testScript = ''
    machine.wait_for_unit("maintainer-mode.service")
    machine.wait_for_open_port(8790)
    machine.succeed("curl -sf http://127.0.0.1:8790/healthz")
    machine.succeed("curl -sf http://127.0.0.1:8790/ | grep -q 'Maintainer Mode'")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1/status | grep -q move_days")
    machine.succeed("curl -sf http://127.0.0.1:8790/static/primer.css >/dev/null")
    machine.succeed("curl -sf http://127.0.0.1:8790/rules -o /tmp/rules && grep -q 'id=\"the-rules\"' /tmp/rules")
    machine.succeed("curl -sf http://127.0.0.1:8790/state | grep -c 'State of nixpkgs'")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1/metrics | grep -c 'concentration_365d'")
    machine.fail("curl -sf http://127.0.0.1:8790/api/v1/people")
    machine.succeed("curl -sf http://127.0.0.1:8790/packages | grep -q 'Packages, modules and areas'")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/packages?q=hello' | grep -q 'Nothing matches'")
    machine.succeed("curl -s http://127.0.0.1:8790/p/hello | grep -q 'first index is still being built'")
    machine.fail("curl -sf http://127.0.0.1:8790/p/hello")
    machine.succeed("curl -sf http://127.0.0.1:8790/a/pkgs/by-name -o /tmp/area && grep -q 'pkgs/by-name/' /tmp/area")
    machine.succeed("curl -sf http://127.0.0.1:8790/u/nobody/maintains | grep -q 'What nobody maintains'")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1/maintainers/nobody | grep -q '\"packages\":\\[\\]'")
    machine.succeed("curl -sf http://127.0.0.1:8790/watch | grep -q 'Sign in to subscribe'")
    machine.fail("curl -sf http://127.0.0.1:8790/feed/nothing")
    machine.succeed("curl -sf http://127.0.0.1:8790/actions | grep -q 'Sign in to run workflows'")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -d pr=1 http://127.0.0.1:8790/actions/review | grep -q 303")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -H 'Sec-Fetch-Site: same-site' -d pr=1 http://127.0.0.1:8790/actions/review | grep -q 403")
    machine.fail("curl -sf http://127.0.0.1:8790/a/..%2Fetc")
    status = machine.execute("maintainer-mode --server http://127.0.0.1:8790 check nobody")[0]
    assert status == 3, f"check exits {status}, not 3"
    machine.succeed("test -f /var/lib/maintainer-mode/maintainer-mode.sqlite")
    machine.wait_until_succeeds("journalctl -u maintainer-mode -o cat > /tmp/log && grep -q 'sync token from GitHub App 1' /tmp/log")
    # the key parsed and the JWT was signed; only the request found no network
    machine.wait_until_succeeds("journalctl -u maintainer-mode -o cat > /tmp/log && grep -q 'GitHub App token: error sending request' /tmp/log")
  '';
}
