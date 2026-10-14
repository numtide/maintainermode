{ self, pkgs }:
pkgs.testers.runNixOSTest {
  name = "maintainer-mode";
  nodes.machine =
    { config, ... }:
    {
      imports = [ self.nixosModules.default ];
      services.maintainer-mode.enable = true;
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
    machine.succeed("curl -sf http://127.0.0.1:8790/rules | grep -q 'id=\"the-rules\"'")
    machine.fail("curl -sf http://127.0.0.1:8790/api/v1/people")
    status = machine.execute("maintainer-mode --server http://127.0.0.1:8790 check nobody")[0]
    assert status == 3, f"check exits {status}, not 3"
    machine.succeed("test -f /var/lib/maintainer-mode/maintainer-mode.sqlite")
  '';
}
