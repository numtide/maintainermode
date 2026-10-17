{ self, pkgs }:
pkgs.testers.runNixOSTest {
  name = "maintainer-mode";
  nodes.machine =
    { config, ... }:
    {
      imports = [ self.nixosModules.default ];
      services.maintainer-mode.enable = true;
      services.maintainer-mode.githubAppId = 1;
      services.maintainer-mode.channels = [
        "nixos-unstable"
        "nixos-26.05"
      ];
      # a throwaway key, made for the test only
      services.maintainer-mode.githubAppKeyFile = pkgs.runCommand "test-github-app-key" {
        nativeBuildInputs = [ pkgs.openssl ];
      } "openssl genrsa -traditional -out $out 2048";
      # a stand-in for sops-nix: /run/secrets links to one generation, and a
      # new secret comes as a new generation
      services.maintainer-mode.syncTokenFile = "/run/secrets/sync-token";
      system.activationScripts.secrets = ''
        mkdir -p /run/secrets.d/1
        echo old-token > /run/secrets.d/1/sync-token
        chmod 0400 /run/secrets.d/1/sync-token
        ln -sfn /run/secrets.d/1 /run/secrets
      '';
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
    machine.succeed("grep -q 'id=\"intro\"' /tmp/page && grep -q 'nixpkgs/blob/master/CONTRIBUTING.md\">nixpkgs contributor guide' /tmp/page")
    machine.succeed("curl -s -D /tmp/intro.h -o /dev/null -X POST http://127.0.0.1:8790/intro/dismiss && grep -q '^HTTP/1.1 303' /tmp/intro.h && grep -qi '^set-cookie: intro=hidden' /tmp/intro.h")
    machine.succeed("curl -sf -H 'Cookie: intro=hidden' http://127.0.0.1:8790/ -o /tmp/page && ! grep -q 'id=\"intro\"' /tmp/page")
    machine.succeed("curl -sf http://127.0.0.1:8790/ -o /tmp/home && grep -q 'Pull requests with no maintainer' /tmp/home && grep -q 'nixpkgs in numbers' /tmp/home")
    machine.succeed("grep -q 'href=\"/review?tab=unowned\"' /tmp/home")
    machine.succeed("curl -s -D /tmp/merge.h -o /dev/null http://127.0.0.1:8790/merge && grep -qi '^location: /review?tab=ready' /tmp/merge.h")
    machine.succeed("curl -s -D /tmp/committer.h -o /dev/null http://127.0.0.1:8790/committer && grep -qi '^location: /review?tab=ready' /tmp/committer.h")
    machine.succeed("curl -s -D /tmp/bumps.h -o /dev/null http://127.0.0.1:8790/bumps && grep -qi '^location: /review?tab=bumps' /tmp/bumps.h")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/review?tab=ready' -o /tmp/merge && grep -q 'aria-current=\"page\">Ready to merge <span class=\"Counter\">0</span>' /tmp/merge")
    machine.succeed("grep -q 'Work in progress.' /tmp/merge && grep -q 'Nothing is ready yet.' /tmp/merge")
    machine.succeed("grep -q 'Maintainer Mode never merges' /tmp/merge && grep -q 'A committer sees the section' /tmp/merge && grep -q 'Approved by a maintainer' /tmp/merge")
    machine.fail("grep -q 'href=\"/committer\"' /tmp/merge")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8790/welcome > /tmp/w1 && grep -q 303 /tmp/w1")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -d done=1 http://127.0.0.1:8790/welcome > /tmp/w2 && grep -q 303 /tmp/w2")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1/status -o /tmp/page && grep -q move_days /tmp/page")
    machine.succeed("curl -sf http://127.0.0.1:8790/static/primer.css >/dev/null")
    machine.succeed("curl -sf http://127.0.0.1:8790/rules -o /tmp/rules && grep -q 'id=\"the-rules\"' /tmp/rules")
    machine.succeed("grep -q 'id=\"contributor-guide\"' /tmp/rules && grep -q 'nixpkgs/blob/master/pkgs/README.md' /tmp/rules")
    machine.succeed("curl -sf http://127.0.0.1:8790/review -o /tmp/review")
    machine.succeed("grep -q 'selected\" href=\"/review?tab=unowned\" aria-current=\"page\">No maintainer <span class=\"Counter\">0</span>' /tmp/review")
    machine.succeed("grep -q 'href=\"/review?tab=bumps\">Bumps' /tmp/review && grep -q 'href=\"/review?tab=ready\">Ready to merge' /tmp/review")
    machine.succeed("grep -q '<title>Review · Maintainer Mode</title>' /tmp/review")
    machine.succeed("grep -q 'href=\"/review\" aria-current=\"page\"' /tmp/review")
    machine.succeed("grep -o 'class=\"UnderlineNav-item\"' /tmp/review | wc -l > /tmp/tabs && grep -qx 4 /tmp/tabs")
    machine.succeed("grep -q 'Data from the last 60 days' /tmp/review")
    machine.fail("grep -q 'nixpkgs index' /tmp/review")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8790/me > /tmp/me1 && grep -q 303 /tmp/me1")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8790/me/runs > /tmp/me2 && grep -q 303 /tmp/me2")
    machine.succeed("curl -s -D /tmp/at.h -o /dev/null 'http://127.0.0.1:8790/packages?q=@nobody' && grep -qi '^location: /u/nobody' /tmp/at.h")
    machine.fail("grep -q -e 'tab=foryou' -e 'your-progress' /tmp/review")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/api/v1/queue/review?bucket=foryou&login=nobody' -o /tmp/q1 && grep -qx '\\[\\]' /tmp/q1")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/api/v1/queue/review?bucket=mine&login=nobody' -o /tmp/q2 && grep -qx '\\[\\]' /tmp/q2")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8790/me/progress > /tmp/p1 && grep -q 303 /tmp/p1")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -d goal=3 http://127.0.0.1:8790/me/progress > /tmp/p2 && grep -q 303 /tmp/p2")
    machine.succeed("curl -sf http://127.0.0.1:8790/state | grep -c 'State of nixpkgs'")
    machine.succeed("curl -sf http://127.0.0.1:8790/state -o /tmp/state && grep -q 'id=\"packages\"' /tmp/state && grep -q 'first nixpkgs index is still being built' /tmp/state")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1/metrics | grep -c 'concentration_365d'")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1/metrics -o /tmp/metrics.json && grep -q '\"open_bumps\":0' /tmp/metrics.json")
    machine.fail("curl -sf http://127.0.0.1:8790/api/v1/people")
    machine.succeed("curl -sf http://127.0.0.1:8790/packages -o /tmp/page && grep -q 'Packages, modules and areas' /tmp/page")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/packages?q=hello' -o /tmp/page && grep -q 'Nothing matches' /tmp/page")
    machine.succeed("curl -s http://127.0.0.1:8790/p/hello -o /tmp/page && grep -q 'first index is still being built' /tmp/page")
    machine.fail("curl -sf http://127.0.0.1:8790/p/hello")
    machine.succeed("curl -sf http://127.0.0.1:8790/a/pkgs/by-name -o /tmp/area && grep -q 'pkgs/by-name/' /tmp/area")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/review?tab=bumps' -o /tmp/bumps && grep -q 'No open version bump' /tmp/bumps && grep -q 'package-updates' /tmp/bumps")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/api/v1/bumps?filter=mine' -o /tmp/bumps.json && grep -q '\"filter\":\"all\"' /tmp/bumps.json")
    machine.succeed("curl -sf http://127.0.0.1:8790/u/nobody/maintains -o /tmp/page && grep -q 'What nobody maintains' /tmp/page")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1/maintainers/nobody -o /tmp/page && grep -q '\"packages\":\\[\\]' /tmp/page")
    machine.succeed("curl -s -D /tmp/watch.h -o /dev/null http://127.0.0.1:8790/watch && grep -q '^HTTP/1.1 308' /tmp/watch.h && grep -qi '^location: /me?tab=watch' /tmp/watch.h")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' 'http://127.0.0.1:8790/me?tab=watch' > /tmp/watch.code && grep -q 303 /tmp/watch.code")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -d name=hello http://127.0.0.1:8790/me/watch > /tmp/watch2.code && grep -q 303 /tmp/watch2.code")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST http://127.0.0.1:8790/me/watch/maintained > /tmp/watch3.code && grep -q 303 /tmp/watch3.code")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1/committer -o /tmp/committer.json && grep -q '\"maintainer_approved\":\\[\\]' /tmp/committer.json")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1/status -o /tmp/status.json && grep -q nixpkgs-committers /tmp/status.json")
    machine.fail("curl -sf http://127.0.0.1:8790/feed/nothing")
    machine.succeed("curl -sf http://127.0.0.1:8790/sync -o /tmp/sync && grep -q 'Faster with forwarded events' /tmp/sync")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1/sync -o /tmp/sync.json && grep -q worst_delay_seconds /tmp/sync.json")
    machine.succeed("grep -q 'CVE Program records' /tmp/sync")
    machine.succeed("grep -q 'its own task' /tmp/sync")
    machine.succeed("grep -q 'Packaging hygiene' /tmp/sync")
    machine.succeed("grep -q 'Upstream tags and registries' /tmp/sync")
    machine.succeed("grep -q 'OSV records' /tmp/sync")
    machine.succeed("grep -q 'at most 4 at a time and 30 a minute' /tmp/sync")
    machine.succeed("grep -q '\"not_asked\":0' /tmp/sync.json && grep -q '\"per_minute\":30' /tmp/sync.json")
    machine.succeed("grep -q with_gaps /tmp/sync.json")
    machine.succeed("grep -q cves_with_score /tmp/sync.json")
    # with no network the tracker names no CVE, so the first severity pass
    # reads nothing and says so
    machine.wait_until_succeeds("curl -sf http://127.0.0.1:8790/api/v1/sync -o /tmp/sync2.json && grep -q '0 records read' /tmp/sync2.json")
    machine.wait_until_succeeds("curl -sf http://127.0.0.1:8790/api/v1/sync -o /tmp/sync3.json && grep -q 'waiting for an index with source URLs' /tmp/sync3.json")
    # the channels wait for a master index, which never comes offline
    machine.succeed("grep -q 'Channel nixos-unstable' /tmp/sync")
    machine.succeed("grep -q 'Channel nixos-26.05</div><div class=\"flex-auto min-width-0\">not read yet' /tmp/sync")
    machine.succeed("grep -q '\"rev\":null' /tmp/sync.json && grep -q '\"channel\":\"nixos-26.05\"' /tmp/sync.json")
    machine.wait_until_succeeds("journalctl -u maintainer-mode -o cat > /tmp/log && grep -q 'channels wait for the master index' /tmp/log")
    machine.succeed("curl -sf http://127.0.0.1:8790/sync -o /tmp/sync && grep -q 'id=\"archive\"' /tmp/sync && grep -q 'waiting for the first listing' /tmp/sync")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1/sync -o /tmp/sync.json && grep -q '\"repo\":\"NixOS/nixpkgs\"' /tmp/sync.json")
    machine.succeed("curl -s -o /tmp/a1 -w '%{http_code}' http://127.0.0.1:8790/api/v1/archive/NixOS/nixpkgs/1 > /tmp/a1.code && grep -q 404 /tmp/a1.code && grep -q 'not in the archive yet' /tmp/a1")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8790/api/v1/archive/NixOS/nix/1 > /tmp/a2.code && grep -q 404 /tmp/a2.code")
    machine.succeed("test -f /var/lib/maintainer-mode/archive/NixOS/nixpkgs.sqlite")
    machine.succeed("maintainer-mode archive export --db /var/lib/maintainer-mode/maintainer-mode.sqlite --repo NixOS/nixpkgs --out /tmp/export 2> /tmp/export.log && grep -q '0 items of NixOS/nixpkgs' /tmp/export.log")
    machine.succeed("curl -sf http://127.0.0.1:8790/actions -o /tmp/page && grep -q 'Sign in to run workflows' /tmp/page")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -d pr=1 http://127.0.0.1:8790/me/runs/review > /tmp/runs1 && grep -q 303 /tmp/runs1")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -H 'Sec-Fetch-Site: same-site' -d pr=1 http://127.0.0.1:8790/me/runs/review > /tmp/runs2 && grep -q 403 /tmp/runs2")
    machine.fail("curl -sf http://127.0.0.1:8790/a/..%2Fetc")
    # the event stream never ends, so curl stops at its time limit (exit 28);
    # curl creates the body file only when data arrives
    machine.succeed("touch /tmp/events")
    status = machine.execute("curl -s -m 3 -D /tmp/events.h -o /tmp/events -H 'Last-Event-ID: 0' 'http://127.0.0.1:8790/api/v1/events?type=PushEvent&number=1&actor=alice'")[0]
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
    machine.succeed("curl -s -D /tmp/r3.h -o /dev/null http://127.0.0.1:8790/me/sets && grep -qi '^location: /me?tab=machines' /tmp/r3.h")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST http://127.0.0.1:8790/me/upload-token > /tmp/r4 && grep -q 303 /tmp/r4")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8790/me/settings > /tmp/r5 && grep -q 303 /tmp/r5")
    machine.succeed("curl -s -D /tmp/r5.h -o /dev/null http://127.0.0.1:8790/me/settings && grep -qi '^cache-control: private, no-store' /tmp/r5.h")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -d review_repo=a/b -d update_repo=a/b -d nixpkgs_fork=a/c http://127.0.0.1:8790/me/settings/workflows > /tmp/r12 && grep -q 303 /tmp/r12")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -d tags=pkgs http://127.0.0.1:8790/me/tags > /tmp/r13 && grep -q 303 /tmp/r13")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST http://127.0.0.1:8790/me/feed > /tmp/r14 && grep -q 303 /tmp/r14")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -d llm_ok=1 http://127.0.0.1:8790/me/settings/llm > /tmp/r6 && grep -q 303 /tmp/r6")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST http://127.0.0.1:8790/me/mcp-token > /tmp/r7 && grep -q 303 /tmp/r7")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' -d '{}' http://127.0.0.1:8790/mcp > /tmp/r8 && grep -q 401 /tmp/r8")
    machine.succeed("curl -s -D /tmp/r9.h -o /dev/null -X POST -H 'Authorization: Bearer nope' -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' -d '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}' http://127.0.0.1:8790/mcp && grep -q '^HTTP/1.1 401' /tmp/r9.h && grep -qi '^www-authenticate: Bearer' /tmp/r9.h")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8790/mcp > /tmp/r10 && grep -q 405 /tmp/r10")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8790/claude/pr/1 > /tmp/r11 && grep -q 303 /tmp/r11")
    # a pull request that is not synced gets a short page with a GitHub link
    machine.succeed("curl -s -o /tmp/pr1 -w '%{http_code}' http://127.0.0.1:8790/pr/1 > /tmp/pr1.code && grep -q 200 /tmp/pr1.code")
    machine.succeed("grep -q 'href=\"https://github.com/NixOS/nixpkgs/pull/1\"' /tmp/pr1 && grep -q 'not in the synced data' /tmp/pr1")
    machine.succeed("curl -s -o /tmp/pr1.json -w '%{http_code}' http://127.0.0.1:8790/api/v1/prs/1 > /tmp/pr1.json.code && grep -q 404 /tmp/pr1.json.code")
    machine.succeed("grep -q '\"url\":\"https://github.com/NixOS/nixpkgs/pull/1\"' /tmp/pr1.json")
    machine.fail("curl -sf http://127.0.0.1:8790/pr/nope")
    # crawlers stay off the pull request pages
    machine.succeed("curl -sf -o /tmp/robots http://127.0.0.1:8790/robots.txt && grep -q '^Disallow: /pr/' /tmp/robots && grep -q '^Disallow: /api/v1/prs/' /tmp/robots")
    # notifications: the inbox and its settings need a session; email is off
    # without SMTP, and an unknown unsubscribe link is not found
    machine.succeed("curl -s -D /tmp/n1.h -o /dev/null http://127.0.0.1:8790/me/inbox && grep -qi '^location: /me?tab=inbox' /tmp/n1.h && grep -qi '^cache-control: private, no-store' /tmp/n1.h")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST http://127.0.0.1:8790/me/inbox/read > /tmp/n2 && grep -q 303 /tmp/n2")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -d k_pr=1 -d email=a@example.org http://127.0.0.1:8790/me/notify > /tmp/n3 && grep -q 303 /tmp/n3")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST http://127.0.0.1:8790/me/notify/test > /tmp/n4 && grep -q 303 /tmp/n4")
    machine.succeed("curl -s -o /tmp/n5.body -w '%{http_code}' http://127.0.0.1:8790/unsubscribe/nope > /tmp/n5 && grep -q 404 /tmp/n5 && grep -q 'unsubscribe link is unknown' /tmp/n5.body")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8790/unsubscribe/0000000000000000000000000000000000000000000000000000000000000000 > /tmp/n6 && grep -q 404 /tmp/n6")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST http://127.0.0.1:8790/unsubscribe/nope > /tmp/n7 && grep -q 404 /tmp/n7")
    machine.succeed("curl -s -o /tmp/n8.body -w '%{http_code}' http://127.0.0.1:8790/confirm/nope > /tmp/n8 && grep -q 404 /tmp/n8 && grep -q 'confirmation link is unknown' /tmp/n8.body")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8790/confirm/0000000000000000000000000000000000000000000000000000000000000000 > /tmp/n9 && grep -q 404 /tmp/n9")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST http://127.0.0.1:8790/confirm/0000000000000000000000000000000000000000000000000000000000000000 > /tmp/n10 && grep -q 404 /tmp/n10")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST http://127.0.0.1:8790/me/notify/confirm > /tmp/n11 && grep -q 303 /tmp/n11")
    machine.fail("systemctl show maintainer-mode -p Environment > /tmp/env && grep -q MAINTAINER_MODE_SMTP /tmp/env")
    # server rescans are off by default: registering a flake is refused
    machine.succeed("curl -s -o /tmp/r5.body -w '%{http_code}' -X POST -d reference=github:o/r -d output=packages.x86_64-linux.default http://127.0.0.1:8790/me/sets/flakes > /tmp/r5 && grep -q 403 /tmp/r5")
    machine.succeed("grep -q 'does not scan flakes' /tmp/r5.body")
    machine.succeed("curl -sf http://127.0.0.1:8790/sync -o /tmp/sync && grep -q 'Flake rescans' /tmp/sync && grep -q 'off on this server' /tmp/sync")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1/sync -o /tmp/sync.json && grep -q '\"rescans\":{\"enabled\":false' /tmp/sync.json")
    machine.succeed("grep -q 'Notification emails' /tmp/sync && grep -q 'did not set up SMTP' /tmp/sync")
    machine.succeed("grep -q '\"email\":{\"enabled\":false,\"per_day\":null,\"sent_24h\":0,\"skipped_24h\":0}' /tmp/sync.json")
    machine.fail("systemctl show maintainer-mode -p Environment > /tmp/env && grep -q MAINTAINER_MODE_RESCAN /tmp/env")
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
    # a new secret reaches the running service through a reload, not a
    # restart, and the value never reaches the log
    machine.succeed("systemctl is-active maintainer-mode-credentials.timer")
    machine.succeed("systemctl start maintainer-mode-credentials.service")
    machine.succeed("journalctl -u maintainer-mode-credentials -o cat > /tmp/cred.log && ! grep -q changed /tmp/cred.log")
    pid = machine.succeed("systemctl show -p MainPID --value maintainer-mode.service").strip()
    machine.succeed("mkdir -p /run/secrets.d/2 && echo new-token > /run/secrets.d/2/sync-token && ln -s /run/secrets.d/2 /run/secrets.tmp && mv -T /run/secrets.tmp /run/secrets && rm -r /run/secrets.d/1")
    # LoadCredential copied the file at start; the copy does not follow
    machine.succeed(f"nsenter -t {pid} -m cat /run/credentials/maintainer-mode.service/sync_token > /tmp/cred && grep -qx old-token /tmp/cred")
    machine.succeed("systemctl start maintainer-mode-credentials.service")
    machine.succeed("journalctl -u maintainer-mode-credentials -o cat > /tmp/cred.log && grep -q 'credential sync_token changed' /tmp/cred.log")
    machine.succeed(f"nsenter -t {pid} -m cat /run/credentials/maintainer-mode.service/sync_token > /tmp/cred && grep -qx new-token /tmp/cred")
    machine.wait_until_succeeds("journalctl -u maintainer-mode -o cat > /tmp/log && grep -q 'sync_token changed; the new secret is in use' /tmp/log")
    machine.succeed(f"systemctl show -p MainPID --value maintainer-mode.service > /tmp/pid && grep -qx {pid} /tmp/pid")
    machine.fail("journalctl -o cat > /tmp/all.log && grep -q -e new-token -e old-token /tmp/all.log")
  '';
}
