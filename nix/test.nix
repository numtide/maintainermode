{ self, pkgs }:
pkgs.testers.runNixOSTest {
  name = "maintainer-mode";
  nodes.machine =
    { config, ... }:
    {
      imports = [ self.nixosModules.default ];
      services.maintainer-mode.enable = true;
      services.maintainer-mode.githubAppId = 1;
      services.maintainer-mode.githubAppSlug = "test-app";
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
      services.maintainer-mode.botTokenFile = "/run/secrets/bot-token";
      system.activationScripts.secrets = ''
        mkdir -p /run/secrets.d/1
        echo old-token > /run/secrets.d/1/bot-token
        chmod 0400 /run/secrets.d/1/bot-token
        ln -sfn /run/secrets.d/1 /run/secrets
      '';
      virtualisation.memorySize = 1024;
      environment.systemPackages = [
        config.services.maintainer-mode.package
        pkgs.sqlite
        pkgs.libxml2
      ];
      # a tree with one private package and a saved public list that knows
      # the other two, so the client exports with no network
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
        {
          top = mk "system" "1" [ private ];
          other = mk "box" "1" [ (mk "hello" "2.12" [ ]) ];
        }
      '';
      environment.etc."scan/index.json".text = builtins.toJSON {
        rev = "test";
        attrs = {
          hello = [
            "hello"
            "2.12"
          ];
          "python3Packages.requests" = [
            "requests"
            "2.31"
          ];
        };
      };
      # an index and an upload token, put in the database while the service
      # is stopped; the token is "vmtoken"
      environment.etc."seed.sql".text =
        let
          pkg = pname: version: {
            inherit pname version;
            file = null;
            maintainers = [ ];
            teams = [ ];
          };
          index = builtins.toJSON {
            rev = "1111111111111111111111111111111111111111";
            maintainers = { };
            owners = [ ];
            pkgs = {
              hello = pkg "hello" "2.12";
              "python3Packages.requests" = pkg "requests" "2.31";
            };
          };
          own = builtins.toJSON {
            number = 575334;
            title = "hello: 2.12 -> 2.12.1";
            url = "https://github.com/NixOS/nixpkgs/pull/575334";
            author = "alice";
            author_is_bot = false;
            state = "OPEN";
            is_draft = false;
            base = "master";
            created_at = "2026-10-10T00:00:00Z";
            updated_at = "2026-10-10T00:00:00Z";
            merged_at = null;
            mergeable = "MERGEABLE";
            head_oid = "1111111111111111111111111111111111111111";
            files = [ ];
            ci = "SUCCESS";
            reviews = [ ];
            author_activity = [ ];
            pushes = [ "2026-10-10T00:00:00Z" ];
            voids = [ ];
          };
        in
        ''
          INSERT OR REPLACE INTO meta (key, value) VALUES ('index', '${index}');
          INSERT OR REPLACE INTO pr (number, updated_at, state, data) VALUES (575334, '2026-10-10T00:00:00Z', 'OPEN', '${own}');
          INSERT OR REPLACE INTO upload_token (login, hash, created_at) VALUES ('alice', 'dcee570f548b91c5fed14f8ada7d88205aad29a2654869a557d3720e982a0b63', '2026-10-01T00:00:00Z');
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
    machine.succeed("grep -q '<h1 class=\"sr-only\">Maintainer Mode</h1>' /tmp/page && ! grep -q 'Subhead-heading' /tmp/page")
    machine.succeed("curl -sf http://127.0.0.1:8790/ -o /tmp/page && grep -o '<h1' /tmp/page | wc -l > /tmp/h1 && grep -qx 1 /tmp/h1 && grep -q 'flex-auto\">Tools that make it easier' /tmp/page")
    machine.succeed("curl -sf http://127.0.0.1:8790/ -o /tmp/home && grep -q 'Pull requests with no maintainer' /tmp/home && grep -q 'nixpkgs in numbers' /tmp/home")
    machine.succeed("grep -q 'href=\"/review?tab=unowned\"' /tmp/home")
    machine.succeed("curl -s -D /tmp/merge.h -o /dev/null http://127.0.0.1:8790/merge && grep -qi '^location: /review?tab=ready' /tmp/merge.h")
    machine.succeed("curl -s -D /tmp/committer.h -o /dev/null http://127.0.0.1:8790/committer && grep -qi '^location: /review?tab=ready' /tmp/committer.h")
    machine.succeed("curl -s -D /tmp/bumps.h -o /dev/null http://127.0.0.1:8790/bumps && grep -qi '^location: /review?tab=bumps' /tmp/bumps.h")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/review?tab=ready' -o /tmp/merge && grep -q 'aria-current=\"page\">Ready to merge <span class=\"Counter\">0</span>' /tmp/merge")
    machine.succeed("grep -q 'Work in progress.' /tmp/merge && grep -q 'Nothing is ready yet.' /tmp/merge")
    machine.succeed("grep -q 'Maintainer Mode never merges' /tmp/merge && grep -q 'A committer sees the section' /tmp/merge && grep -q 'Approved by a maintainer' /tmp/merge")
    machine.fail("grep -q 'href=\"/committer\"' /tmp/merge")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/review?tab=ready&section=security' -o /tmp/sec && grep -q 'Back to Ready to merge' /tmp/sec && grep -q 'No security fix is ready.' /tmp/sec")
    machine.succeed("! grep -q 'Waiting longest' /tmp/sec && ! grep -q 'A committer sees the section' /tmp/sec")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/review?tab=ready&section=nonsense' -o /tmp/sec2 && grep -q 'Waiting longest' /tmp/sec2")
    machine.succeed("curl -s -D /tmp/w1.h -o /dev/null http://127.0.0.1:8790/welcome && grep -q '^HTTP/1.1 308' /tmp/w1.h && grep -qi '^location: /#start' /tmp/w1.h")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -d done=1 http://127.0.0.1:8790/me/start > /tmp/w2 && grep -q 303 /tmp/w2")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1/status -o /tmp/page && grep -q move_days /tmp/page")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/state?tab=rules' -o /tmp/rules && grep -q 'id=\"the-rules\"' /tmp/rules")
    machine.succeed("grep -q 'id=\"contributor-guide\"' /tmp/rules && grep -q 'nixpkgs/blob/master/pkgs/README.md' /tmp/rules")
    machine.succeed("grep -q 'selected\" href=\"/state?tab=rules\" aria-current=\"page\">Rules</a>' /tmp/rules")
    machine.succeed("curl -s -D /tmp/rules.h -o /dev/null http://127.0.0.1:8790/rules && grep -qi '^location: /state?tab=rules' /tmp/rules.h")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/state?tab=docs' -o /tmp/docs && grep -q 'href=\"/state?tab=docs&amp;doc=health\"' /tmp/docs")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/state?tab=docs&doc=personal' -o /tmp/doc && grep -q 'Your own view</h1>' /tmp/doc && grep -q 'href=\"/state?tab=docs&amp;doc=health#the-risk-score\"' /tmp/doc")
    machine.succeed("curl -s -D /tmp/sync.h -o /dev/null http://127.0.0.1:8790/sync && grep -qi '^location: /state?tab=status' /tmp/sync.h")
    machine.succeed("curl -sf http://127.0.0.1:8790/review -o /tmp/review")
    machine.succeed("grep -q 'selected\" href=\"/review?tab=unowned\" aria-current=\"page\">No maintainer <span class=\"Counter\">0</span>' /tmp/review")
    machine.succeed("grep -q 'href=\"/review?tab=bumps\">Bumps' /tmp/review && grep -q 'href=\"/review?tab=ready\">Ready to merge' /tmp/review")
    machine.succeed("grep -q '<title>Review · Maintainer Mode</title>' /tmp/review")
    # style sheets carry a hash in their name and stay cached for a year
    machine.succeed("grep -o 'href=\"/static/primer\\.[0-9a-f]*\\.css\"' /tmp/review | cut -d'\"' -f2 > /tmp/css.path")
    machine.succeed("curl -sf -D /tmp/css.h -o /tmp/css http://127.0.0.1:8790$(cat /tmp/css.path) && grep -qi '^cache-control: public, max-age=31536000, immutable' /tmp/css.h")
    machine.succeed("grep -q 'data-dark-theme=dark' /tmp/css && ! grep -q dark_dimmed /tmp/css")
    machine.fail("curl -sf http://127.0.0.1:8790/static/primer.css")
    # icons point into one hidden sprite on the page
    machine.succeed("grep -q '<use href=\"#i-eye\">' /tmp/review && grep -q '<symbol id=\"i-eye\"' /tmp/review")
    machine.succeed("curl -s -H 'Accept-Encoding: gzip' -D /tmp/gz.h -o /dev/null http://127.0.0.1:8790/review && grep -qi '^content-encoding: gzip' /tmp/gz.h")
    # a public page sends an ETag and answers 304 while the data stays the
    # same; a background pass can change the data between the two requests
    machine.wait_until_succeeds("curl -s -D /tmp/etag.h -o /dev/null http://127.0.0.1:8790/review && grep -i '^etag: W/' /tmp/etag.h | cut -d' ' -f2 | tr -d '\\r' > /tmp/etag && curl -s -o /dev/null -w '%{http_code}' -H \"If-None-Match: $(cat /tmp/etag)\" http://127.0.0.1:8790/review > /tmp/etag.code && grep -qx 304 /tmp/etag.code")
    machine.succeed("curl -s -D /tmp/sync.h -o /dev/null 'http://127.0.0.1:8790/state?tab=status' && ! grep -qi '^etag' /tmp/sync.h")
    machine.succeed("grep -q 'href=\"/review\" aria-current=\"page\"' /tmp/review")
    machine.succeed("grep -o 'class=\"UnderlineNav-item\"' /tmp/review | wc -l > /tmp/tabs && grep -qx 4 /tmp/tabs")
    machine.succeed("grep -q 'Data from the last 60 days' /tmp/review")
    machine.fail("grep -q 'nixpkgs index' /tmp/review")
    machine.succeed("curl -s -D /tmp/me1.h -o /dev/null http://127.0.0.1:8790/me && grep -q '^HTTP/1.1 308' /tmp/me1.h && grep -qi '^location: /packages?filter=mine' /tmp/me1.h")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8790/me/runs > /tmp/me2 && grep -q 303 /tmp/me2")
    machine.succeed("curl -sf http://127.0.0.1:8790/actions -o /tmp/actions && grep -q 'href=\"https://github.com/apps/test-app\"' /tmp/actions")
    machine.succeed("curl -s -D /tmp/at.h -o /dev/null 'http://127.0.0.1:8790/packages?q=@nobody' && grep -qi '^location: /u/nobody' /tmp/at.h")
    machine.fail("grep -q -e 'tab=foryou' -e 'your-progress' /tmp/review")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1 -o /tmp/api.json && grep -q '/api/v1/prs' /tmp/api.json && grep -q '/api/v1/people/<login>' /tmp/api.json")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/api/v1/prs?queue=foryou&login=nobody' -o /tmp/q1 && grep -q '\"items\":\\[\\]' /tmp/q1 && grep -q '\"next_cursor\":null' /tmp/q1")
    machine.succeed("curl -s -o /tmp/q2 -w '%{http_code}' 'http://127.0.0.1:8790/api/v1/prs?queue=foryou' > /tmp/q2.code && grep -q 400 /tmp/q2.code && grep -q 'needs login' /tmp/q2")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/api/v1/prs?queue=ready&section=maintainer_approved' -o /tmp/q3 && grep -q '\"section\":\"maintainer_approved\"' /tmp/q3")
    machine.succeed("maintainer-mode --server http://127.0.0.1:8790 --json prs --queue unowned > /tmp/q4 && grep -q '\"queue\":\"unowned\"' /tmp/q4")
    machine.fail("curl -sf http://127.0.0.1:8790/api/v1/queue/merge")
    machine.fail("curl -sf http://127.0.0.1:8790/api/v1/committer")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8790/me/progress > /tmp/p1 && grep -q 303 /tmp/p1")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -d goal=3 http://127.0.0.1:8790/me/progress > /tmp/p2 && grep -q 303 /tmp/p2")
    machine.succeed("curl -sf http://127.0.0.1:8790/state | grep -c 'State of nixpkgs'")
    machine.succeed("curl -sf http://127.0.0.1:8790/state -o /tmp/state && grep -q 'id=\"packages\"' /tmp/state && grep -q 'first nixpkgs index is still being built' /tmp/state")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1/metrics | grep -c 'concentration_365d'")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1/metrics -o /tmp/metrics.json && grep -q '\"open_bumps\":0' /tmp/metrics.json")
    machine.fail("curl -sf http://127.0.0.1:8790/api/v1/people")
    machine.succeed("curl -sf -D /tmp/page.h http://127.0.0.1:8790/packages -o /tmp/page && grep -q 'Packages, modules and directories' /tmp/page")
    # a visitor sees no filter bar and nothing private
    machine.succeed("! grep -q -e 'filter=' -e 'Upload the packages' /tmp/page && ! grep -qi '^cache-control: private' /tmp/page.h")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/packages?q=hello' -o /tmp/page && grep -q 'Nothing matches' /tmp/page")
    machine.succeed("curl -s http://127.0.0.1:8790/p/hello -o /tmp/page && grep -q 'first index is still being built' /tmp/page")
    machine.fail("curl -sf http://127.0.0.1:8790/p/hello")
    machine.succeed("curl -sf http://127.0.0.1:8790/a/pkgs/by-name -o /tmp/area && grep -q 'pkgs/by-name/' /tmp/area")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/review?tab=bumps' -o /tmp/bumps && grep -q 'No open version bump' /tmp/bumps && grep -q 'package-updates' /tmp/bumps")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/api/v1/prs?queue=bumps' -o /tmp/bumps.json && grep -q '\"queue\":\"bumps\"' /tmp/bumps.json && grep -q '\"total\":0' /tmp/bumps.json")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8790/u/nobody > /tmp/person.code && grep -q 200 /tmp/person.code")
    machine.succeed("curl -sf http://127.0.0.1:8790/u/nobody -o /tmp/person && grep -q 'selected\" href=\"/u/nobody\" aria-current=\"page\">Overview</a>' /tmp/person && grep -q 'href=\"/u/nobody?tab=reviews\">Reviews</a>' /tmp/person && grep -q 'href=\"/u/nobody?tab=maintains\">Maintains</a>' /tmp/person")
    machine.succeed("! grep -q 'href=\"/packages\" aria-current' /tmp/person && grep -q 'avatars.githubusercontent.com/nobody?s=96' /tmp/person && grep -q 'No open pull request in the synced data.' /tmp/person")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/u/nobody?tab=reviews' -o /tmp/page && grep -q 'aria-current=\"page\">Reviews</a>' /tmp/page && grep -q 'facts about reviews, not a judgement' /tmp/page")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/u/nobody?tab=maintains' -o /tmp/page && grep -q 'What nobody maintains' /tmp/page && grep -q 'aria-current=\"page\">Maintains</a>' /tmp/page")
    machine.succeed("curl -s -D /tmp/um.h -o /dev/null http://127.0.0.1:8790/u/nobody/maintains && grep -q '^HTTP/1.1 308' /tmp/um.h && grep -qi '^location: /u/nobody?tab=maintains' /tmp/um.h")
    machine.succeed("curl -s -o /dev/null -w '%{redirect_url}' 'http://127.0.0.1:8790/record?login=nobody' > /tmp/record.loc && grep -q '/u/nobody$' /tmp/record.loc")
    machine.succeed("curl -s -o /dev/null -w '%{redirect_url}' http://127.0.0.1:8790/record > /tmp/record.loc && grep -q '/packages$' /tmp/record.loc")
    machine.succeed("curl -s -D /tmp/watch.h -o /dev/null http://127.0.0.1:8790/watch && grep -q '^HTTP/1.1 308' /tmp/watch.h && grep -qi '^location: /packages?filter=watch' /tmp/watch.h")
    for tab, to in [("inbox", "/inbox"), ("maintain", "/packages?filter=maintain"), ("watch", "/packages?filter=watch"), ("machines", "/packages?filter=machines")]:
        machine.succeed(f"curl -s -D /tmp/old-{tab}.h -o /dev/null 'http://127.0.0.1:8790/me?tab={tab}' && grep -q '^HTTP/1.1 308' /tmp/old-{tab}.h && grep -qi '^location: {to}\\s*$' /tmp/old-{tab}.h")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -d name=hello http://127.0.0.1:8790/me/watch > /tmp/watch2.code && grep -q 303 /tmp/watch2.code")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST http://127.0.0.1:8790/me/watch/maintained > /tmp/watch3.code && grep -q 303 /tmp/watch3.code")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1/people/nobody -o /tmp/page && grep -q '\"packages\":\\[\\]' /tmp/page && grep -q '\"dossier\"' /tmp/page && grep -q '\"open_prs\":\\[\\]' /tmp/page && grep -q '\"facts\"' /tmp/page")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1/status -o /tmp/status.json && grep -q nixpkgs-committers /tmp/status.json")
    machine.fail("curl -sf http://127.0.0.1:8790/feed/nothing")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/state?tab=status' -o /tmp/sync && grep -q 'How fast changes show here' /tmp/sync")
    # the bot token turns the notifications poller on; with no network its
    # polls fail, so the page says so and the full walk keeps its interval
    machine.succeed("grep -q 'Notifications</div><div class=\"flex-auto min-width-0\">the notifications of the bot account' /tmp/sync")
    machine.succeed("grep -q 'the poll of its notifications fails now' /tmp/sync")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1/sync -o /tmp/sync.json && grep -q worst_delay_seconds /tmp/sync.json")
    machine.succeed("grep -q '\"notifications\":{[^}]*\"enabled\":true' /tmp/sync.json && grep -q '\"walk_every_seconds\":600' /tmp/sync.json")
    machine.wait_until_succeeds("curl -sf 'http://127.0.0.1:8790/state?tab=status' -o /tmp/sync.n && grep -q 'errors in the last hour; the last: ' /tmp/sync.n")
    machine.succeed("journalctl -u maintainer-mode -o cat > /tmp/log && grep -q 'notifications poller on' /tmp/log")
    machine.succeed("systemctl show maintainer-mode -p Environment > /tmp/env && grep -q 'MAINTAINER_MODE_BOT_TOKEN_FILE=' /tmp/env && ! grep -q SYNC_TOKEN /tmp/env")
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
    machine.succeed("curl -sf 'http://127.0.0.1:8790/state?tab=status' -o /tmp/sync && grep -q 'id=\"archive\"' /tmp/sync && grep -q 'waiting for the first listing' /tmp/sync")
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
    machine.succeed("curl -s -D /tmp/r3.h -o /dev/null http://127.0.0.1:8790/me/sets && grep -qi '^location: /packages?filter=machines' /tmp/r3.h")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST http://127.0.0.1:8790/me/upload-token > /tmp/r4 && grep -q 303 /tmp/r4")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8790/me/settings > /tmp/r5 && grep -q 303 /tmp/r5")
    machine.succeed("curl -s -D /tmp/r5.h -o /dev/null http://127.0.0.1:8790/me/settings && grep -qi '^cache-control: private, no-store' /tmp/r5.h")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -d review_repo=a/b -d update_repo=a/b -d nixpkgs_fork=a/c http://127.0.0.1:8790/me/settings/workflows > /tmp/r12 && grep -q 303 /tmp/r12")
    machine.succeed("curl -s -D /tmp/check.h -o /dev/null -X POST http://127.0.0.1:8790/me/settings/workflows/check && grep -q '^HTTP/1.1 303' /tmp/check.h && grep -qi '^cache-control: private, no-store' /tmp/check.h")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -d tags=pkgs http://127.0.0.1:8790/me/tags > /tmp/r13 && grep -q 303 /tmp/r13")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST http://127.0.0.1:8790/me/feed > /tmp/r14 && grep -q 303 /tmp/r14")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -d llm_ok=1 http://127.0.0.1:8790/me/settings/llm > /tmp/r6 && grep -q 303 /tmp/r6")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST http://127.0.0.1:8790/me/mcp-token > /tmp/r7 && grep -q 303 /tmp/r7")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' -d '{}' http://127.0.0.1:8790/mcp > /tmp/r8 && grep -q 401 /tmp/r8")
    machine.succeed("curl -s -D /tmp/r9.h -o /dev/null -X POST -H 'Authorization: Bearer nope' -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' -d '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}' http://127.0.0.1:8790/mcp && grep -q '^HTTP/1.1 401' /tmp/r9.h && grep -qi '^www-authenticate: Bearer' /tmp/r9.h")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8790/mcp > /tmp/r10 && grep -q 405 /tmp/r10")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8790/claude/pr/1 > /tmp/r11 && grep -q 303 /tmp/r11")
    # a pull request that is not in the data gets a short page with a GitHub link
    machine.succeed("curl -s -o /tmp/pr1 -w '%{http_code}' http://127.0.0.1:8790/pr/1 > /tmp/pr1.code && grep -q 200 /tmp/pr1.code")
    machine.succeed("grep -q 'href=\"https://github.com/NixOS/nixpkgs/pull/1\"' /tmp/pr1 && grep -q 'not in the data of this site' /tmp/pr1")
    machine.succeed("! grep -q 'id=\"decide\"' /tmp/pr1")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8790/pr/18446744073709551615 > /tmp/pr2.code && grep -q 200 /tmp/pr2.code && curl -sf http://127.0.0.1:8790/healthz")
    machine.succeed("curl -s -o /tmp/pr1.json -w '%{http_code}' http://127.0.0.1:8790/api/v1/prs/1 > /tmp/pr1.json.code && grep -q 404 /tmp/pr1.json.code")
    machine.succeed("grep -q '\"url\":\"https://github.com/NixOS/nixpkgs/pull/1\"' /tmp/pr1.json")
    machine.fail("curl -sf http://127.0.0.1:8790/pr/nope")
    # crawlers stay off the pull request pages
    machine.succeed("curl -sf -o /tmp/robots http://127.0.0.1:8790/robots.txt && grep -q '^Disallow: /pr/' /tmp/robots && grep -q '^Disallow: /api/v1/prs/' /tmp/robots")
    # notifications: the inbox and its settings need a session; email is off
    # without SMTP, and an unknown unsubscribe link is not found
    machine.succeed("curl -s -D /tmp/n1.h -o /dev/null http://127.0.0.1:8790/me/inbox && grep -qi '^location: /inbox' /tmp/n1.h && grep -qi '^cache-control: private, no-store' /tmp/n1.h")
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
    machine.succeed("curl -s -o /tmp/r5.body -w '%{http_code}' -X POST -d name=web -d reference=github:o/r -d output=packages.x86_64-linux.default http://127.0.0.1:8790/me/sets/flakes > /tmp/r5 && grep -q 403 /tmp/r5")
    machine.succeed("grep -q 'does not scan flakes' /tmp/r5.body")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/state?tab=status' -o /tmp/sync && grep -q 'Flake rescans' /tmp/sync && grep -q 'off on this server' /tmp/sync")
    machine.succeed("curl -sf http://127.0.0.1:8790/api/v1/sync -o /tmp/sync.json && grep -q '\"rescans\":{\"enabled\":false' /tmp/sync.json")
    machine.succeed("grep -q 'Notification emails' /tmp/sync && grep -q 'did not set up SMTP' /tmp/sync")
    machine.succeed("grep -q '\"email\":{\"enabled\":false,\"per_day\":null,\"sent_24h\":0,\"skipped_24h\":0}' /tmp/sync.json")
    machine.fail("systemctl show maintainer-mode -p Environment > /tmp/env && grep -q MAINTAINER_MODE_RESCAN /tmp/env")
    machine.succeed("cp -rL /etc/scan /tmp/scan")
    # before the first index, the public list answers 503 and export stops
    machine.succeed("curl -s -o /tmp/pi -w '%{http_code}' http://127.0.0.1:8790/api/v1/packages/index > /tmp/pi.code && grep -q 503 /tmp/pi.code && grep -q 'not built yet' /tmp/pi")
    machine.succeed("cd /tmp/scan && maintainer-mode scan --file host.nix --attr top")
    machine.succeed("grep -q our-private-daemon /tmp/scan/maintainer-mode-scan.json")
    machine.fail("cd /tmp/scan && maintainer-mode --server http://127.0.0.1:8790 export --out report.json")
    machine.succeed("cd /tmp/scan && maintainer-mode export --index-file index.json --out report.json")
    machine.succeed("grep -q '\"attr\": \"python3Packages.requests\"' /tmp/scan/report.json && grep -q '\"modified\": true' /tmp/scan/report.json")
    machine.succeed("grep -q '\"version\": \"2.12\"' /tmp/scan/report.json && grep -q '\"index_rev\": \"test\"' /tmp/scan/report.json")
    machine.succeed("grep -q '\"unknown\": 2' /tmp/scan/report.json")
    machine.fail("grep -q -e our-private-daemon -e host.nix -e '\"2.32\"' -e '\"name\"' -e '\"outputs\"' /tmp/scan/report.json")
    # two outputs: the local report keeps the outputs and the derivations;
    # only --with-outputs sends the labels, and never a store path
    machine.succeed("cd /tmp/scan && maintainer-mode scan --file host.nix --attr top --attr other --out multi.json")
    machine.succeed("grep -q '\"label\": \"other\"' /tmp/scan/multi.json && grep -q '/nix/store/.*-hello-2.12.drv' /tmp/scan/multi.json")
    machine.succeed("cd /tmp/scan && maintainer-mode export --index-file index.json --report multi.json --with-outputs --out multi-report.json")
    machine.succeed("tr -d ' \\n' < /tmp/scan/multi-report.json > /tmp/mr && grep -q '\"outputs\":\\[\"top\",\"other\"\\]' /tmp/mr && grep -q '\"attr\":\"hello\",\"pname\":\"hello\",\"version\":\"2.12\",\"outputs\":\\[0,1\\]' /tmp/mr")
    machine.fail("grep -q -e /nix/store -e host.nix -e installable /tmp/scan/multi-report.json")
    machine.succeed("cd /tmp/scan && maintainer-mode export --index-file index.json --report multi.json --out multi-plain.json && ! grep -q outputs /tmp/scan/multi-plain.json")
    # why runs nix here, from each output to the derivation of the package
    machine.succeed("cd /tmp/scan && maintainer-mode why hello --report multi.json > /tmp/why.out && grep -q 'top holds hello-2.12' /tmp/why.out && grep -q 'other holds hello-2.12' /tmp/why.out && grep -q 'our-private-daemon-1.0.drv' /tmp/why.out")
    machine.succeed("cd /tmp/scan && maintainer-mode why hello --report multi.json --output other > /tmp/why2.out && ! grep -q 'top holds' /tmp/why2.out")
    machine.fail("cd /tmp/scan && maintainer-mode why hello --report multi.json --output nope")
    machine.fail("cd /tmp/scan && maintainer-mode submit --to http://127.0.0.1:8790")
    machine.fail("cd /tmp/scan && MAINTAINER_MODE_UPLOAD_TOKEN=nope maintainer-mode submit --to http://127.0.0.1:8790")
    machine.succeed("test -f /var/lib/maintainer-mode/maintainer-mode.sqlite")
    # sign in without GitHub: a session row with a dummy token. The dynamic
    # service user cannot reach /var/lib/private, so root writes the row and
    # gives the WAL files back to the service user
    db = "/var/lib/maintainer-mode/maintainer-mode.sqlite"
    machine.succeed(
        f"sqlite3 -cmd '.timeout 5000' {db} "
        "\"INSERT INTO session (id, login, token, created_at, seen_at) "
        "VALUES ('vm-session', 'alice', 'vm-dummy-token', strftime('%Y-%m-%dT%H:%M:%SZ'), strftime('%Y-%m-%dT%H:%M:%SZ'))\""
        f" && chown --reference={db} {db}-wal {db}-shm"
    )
    base = "http://127.0.0.1:8790"
    cookie = "-H 'Cookie: session=vm-session'"

    def signed_in(path, out, code=200):
        machine.succeed(f"curl -s {cookie} -D {out}.h -o {out} -w '%{{http_code}}' '{base}{path}' > {out}.code && grep -qx {code} {out}.code")
        machine.succeed(f"grep -qi '^cache-control: private, no-store' {out}.h")

    def form(path, out, data="", code=303):
        machine.succeed(f"curl -s {cookie} -D {out}.h -o {out} -w '%{{http_code}}' -X POST {data} '{base}{path}' > {out}.code && grep -qx {code} {out}.code")
        machine.succeed(f"grep -qi '^cache-control: private, no-store' {out}.h")

    for path, out in [("/", "home"), ("/inbox", "inbox"), ("/me/settings", "settings"), ("/me/runs", "runs"),
                      ("/me/progress", "progress"), ("/review", "review"), ("/review?tab=ready", "ready"),
                      ("/u/alice", "u"), ("/packages", "packages"), ("/state", "state")]:
        signed_in(path, f"/tmp/s-{out}")
        machine.succeed(f"grep -q 'menu-login\">alice<' /tmp/s-{out}")
    machine.succeed("grep -q 'Needs you <span class=\"Counter\">' /tmp/s-home && grep -q 'aria-label=\"Moods\"' /tmp/s-home && grep -q 'Your packages that need you' /tmp/s-home && grep -q 'href=\"/packages?filter=mine\"' /tmp/s-home")
    machine.succeed("! grep -q 'Pull requests for you to review' /tmp/s-home && grep -q 'Keys: <kbd>j</kbd>' /tmp/s-home")
    machine.succeed("curl -sf -o /tmp/pub-home http://127.0.0.1:8790/ && ! grep -q -e 'Needs you' -e '<script>' /tmp/pub-home")
    signed_in("/?mood=review", "/tmp/s-mood")
    machine.succeed("grep -q 'aria-current=\"page\">Review <span class=\"Counter\">' /tmp/s-mood && grep -qi '^set-cookie: mood=review' /tmp/s-mood.h")
    signed_in("/needs/next?mood=mine&from=pr:1", "/tmp/s-next", 303)
    machine.succeed("grep -qi '^location: /?mood=mine#needs' /tmp/s-next.h && grep -qi '^set-cookie: mood=mine' /tmp/s-next.h")
    form("/needs/snooze", "/tmp/f-snooze", "-d key=pr:1 -d fingerprint=0123456789abcdef -d mood=mine")
    machine.succeed("grep -qi '^location: /?mood=mine#needs' /tmp/f-snooze.h")
    form("/needs/snooze", "/tmp/f-snooze2", "-d key=pr:1 -d fingerprint=0123456789abcdef -d then=next")
    machine.succeed("grep -qi '^location: /needs/next?mood=' /tmp/f-snooze2.h")
    signed_in("/api/v1/needs?mood=mine", "/tmp/s-needs.json")
    machine.succeed("grep -q '\"login\":\"alice\"' /tmp/s-needs.json && grep -q '\"items\":\\[\\]' /tmp/s-needs.json && grep -q '\"mood\":\"mine\"' /tmp/s-needs.json")
    machine.succeed("curl -s -D /tmp/needs0.h -o /dev/null http://127.0.0.1:8790/api/v1/needs && grep -q '^HTTP/1.1 401' /tmp/needs0.h && grep -qi '^cache-control: private, no-store' /tmp/needs0.h")
    machine.succeed("grep -q '<a href=\"/inbox\" aria-current=\"page\">' /tmp/s-inbox && ! grep -q 'UnderlineNav-item\" href=\"[^\"]*\" aria-current' /tmp/s-inbox")
    machine.succeed("grep -q '<a href=\"/me/settings\" aria-current=\"page\">' /tmp/s-settings && ! grep -q 'UnderlineNav-item\" href=\"[^\"]*\" aria-current' /tmp/s-settings")
    machine.succeed("curl -s -D /tmp/inbox0.h -o /dev/null http://127.0.0.1:8790/inbox && grep -q '^HTTP/1.1 303' /tmp/inbox0.h && grep -qi '^cache-control: private, no-store' /tmp/inbox0.h")
    for f, label in [("mine", "Mine"), ("machines", "On my machines"), ("maintain", "I maintain"), ("watch", "I watch"), ("all", "All of nixpkgs")]:
        signed_in(f"/packages?filter={f}", f"/tmp/s-pk-{f}")
        machine.succeed(f"grep -q 'href=\"/packages?filter={f}\" aria-current=\"page\">{label} <span class=\"Counter\">' /tmp/s-pk-{f}")
        machine.succeed(f"grep -q 'href=\"/packages\" aria-current=\"page\"' /tmp/s-pk-{f}")
        machine.succeed(f"grep -q 'href=\"/me/settings#upload\">Upload the packages of your machines' /tmp/s-pk-{f}")
    machine.succeed("grep -q 'No report yet.' /tmp/s-pk-machines && grep -q 'id=\"flakes\"' /tmp/s-pk-machines")
    machine.succeed("grep -q 'Box-title\">Directories</h2>' /tmp/s-pk-all && grep -q 'name=\"filter\" value=\"all\"' /tmp/s-pk-all")
    machine.succeed("grep -q 'action=\"/me/watch\"' /tmp/s-pk-watch")
    for anchor in ["email", "followup", "feed", "upload", "llm", "mcp", "workflows", "tags", "goal", "start"]:
        machine.succeed(f"grep -q '<div id=\"{anchor}\">' /tmp/s-settings && grep -q 'href=\"#{anchor}\"' /tmp/s-settings")
    machine.succeed("grep -q 'Runs</h1>' /tmp/s-runs && grep -q 'Your weekly goal' /tmp/s-progress")
    machine.succeed("grep -q 'selected\" href=\"/review?tab=foryou\" aria-current=\"page\">For you' /tmp/s-review")
    machine.succeed("grep -q 'href=\"/review?tab=ready\" aria-current=\"page\">Ready to merge' /tmp/s-ready && grep -q 'href=\"/review?tab=foryou\">For you' /tmp/s-ready")
    machine.succeed("grep -q 'aria-current=\"page\">Mine <span' /tmp/s-packages")
    # the forms: the index is empty offline, so a package name is refused and
    # a directory is watched
    form("/me/watch", "/tmp/f-watch0", "-d name=hello", 400)
    form("/me/watch", "/tmp/f-watch", "-d name=pkgs/development/vm-watched")
    machine.succeed("grep -qi '^location: /packages?filter=watch' /tmp/f-watch.h")
    signed_in("/packages?filter=watch", "/tmp/s-watch")
    machine.succeed("grep -q 'value=\"area:pkgs/development/vm-watched\"' /tmp/s-watch")
    form("/me/watch/remove", "/tmp/f-unwatch", "-d key=area:pkgs/development/vm-watched")
    signed_in("/packages?filter=watch", "/tmp/s-watch2")
    machine.fail("grep -q 'vm-watched' /tmp/s-watch2")
    form("/me/watch", "/tmp/f-watch2", "-d name=pkgs/development/vm-watched")
    form("/me/inbox/read", "/tmp/f-read")
    form("/me/tags", "/tmp/f-tags", "-d tags=pkgs/vm-tag")
    signed_in("/me/settings", "/tmp/s-settings2")
    machine.succeed("grep -q '>pkgs/vm-tag</textarea>' /tmp/s-settings2")
    form("/me/progress", "/tmp/f-goal", "-d goal=4")
    signed_in("/me/progress", "/tmp/s-progress2")
    machine.succeed("grep -q 'of 4' /tmp/s-progress2")
    form("/me/progress", "/tmp/f-goal2", "-d goal=many", 400)
    # the default follow-up plan of your pull requests; a plan needs a
    # pull request of yours in the synced data
    machine.succeed("grep -q 'action=\"/me/settings/followup\"' /tmp/s-settings && grep -q 'Not saved yet' /tmp/s-settings")
    form("/me/settings/followup", "/tmp/f-fdef", "-d days=10 -d action=ask")
    machine.succeed("grep -qi '^location: /me/settings#followup' /tmp/f-fdef.h")
    form("/me/settings/followup", "/tmp/f-fdef2", "-d days=0 -d action=ask", 400)
    signed_in("/me/settings", "/tmp/s-settings5")
    machine.succeed("grep -q 'id=\"followup-default-days\" name=\"days\" value=\"10\"' /tmp/s-settings5 && grep -q 'Saved.' /tmp/s-settings5")
    form("/me/followup", "/tmp/f-plan0", "-d number=575334 -d days=5 -d action=merge", 400)
    form("/me/settings/llm", "/tmp/f-llm", "-d llm_ok=1")
    signed_in("/me/settings", "/tmp/s-settings3")
    machine.succeed("grep -q 'name=\"llm_ok\" value=\"1\" checked' /tmp/s-settings3")
    form("/me/feed", "/tmp/f-feed")
    signed_in("/me/settings", "/tmp/s-settings4")
    machine.succeed("grep -o '/feed/[0-9a-f]*' /tmp/s-settings4 | head -1 > /tmp/feed.path && test -s /tmp/feed.path")
    machine.succeed(f"curl -sf -o /tmp/feed.xml {base}$(cat /tmp/feed.path) && xmllint --noout /tmp/feed.xml")
    machine.succeed("grep -q '<feed xmlns=\"http://www.w3.org/2005/Atom\">' /tmp/feed.xml && grep -q '<id>http://' /tmp/feed.xml && grep -q '<updated>' /tmp/feed.xml")
    form("/me/upload-token", "/tmp/f-upload", code=200)
    machine.succeed("grep -o 'value=\"[0-9a-f]\\{64\\}\" aria-label=\"upload token\"' /tmp/f-upload | cut -d'\"' -f2 > /tmp/upload.token && test -s /tmp/upload.token")
    # login: the tool waits until alice confirms its code, then saves a token only she can read
    machine.succeed("mkdir -p /tmp/lh")
    machine.succeed(f"HOME=/tmp/lh maintainer-mode --server {base} login < /dev/null > /dev/null 2> /tmp/login.err &")
    code = machine.wait_until_succeeds("grep -o 'code [A-Z]\\{4\\}-[A-Z]\\{4\\}' /tmp/login.err | cut -d' ' -f2 | grep .").strip()
    signed_in(f"/login/cli?code={code}", "/tmp/s-cli")
    machine.succeed(f"grep -q 'value=\"{code}\" autocomplete' /tmp/s-cli")
    form("/login/cli", "/tmp/f-cli-bad", "-d code=BBBB-BBBB", 404)
    form("/login/cli", "/tmp/f-cli", f"-d code={code.lower()}", 200)
    machine.wait_until_succeeds("grep -q 'Signed in as alice' /tmp/login.err")
    machine.succeed("test $(stat -c %a /tmp/lh/.config/maintainer-mode/upload-token) = 600")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' -d '{\"device_code\":\"x\"}' http://127.0.0.1:8790/api/v1/login/poll > /tmp/poll.code && grep -qx 404 /tmp/poll.code")
    machine.succeed("curl -s -D /tmp/cli-out.h -o /dev/null 'http://127.0.0.1:8790/login/cli?code=bbbb-bbbb' && grep -qi '^location: /auth/login?next=%2Flogin%2Fcli%3Fcode%3DBBBB-BBBB' /tmp/cli-out.h")
    # email needs SMTP, which the VM does not have, so root stores the address
    machine.succeed(
        f"sqlite3 -cmd '.timeout 5000' {db} "
        "\"INSERT OR REPLACE INTO setting (login, key, value) VALUES ('alice', 'digest_email', 'vm-mail@example.org')\""
        f" && sqlite3 -cmd '.timeout 5000' {db} \"SELECT value FROM setting WHERE login = 'alice' AND key = 'digest_email'\" > /tmp/mail && grep -qx vm-mail@example.org /tmp/mail"
        f" && chown --reference={db} {db}-wal {db}-shm"
    )
    # a public page, fetched with no cookie, shows nothing private
    machine.succeed(f"curl -sf -D /tmp/pub.h -o /tmp/pub {base}/u/alice && curl -sf -o /tmp/pub.json {base}/api/v1/people/alice")
    machine.fail("grep -qi '^cache-control: private' /tmp/pub.h")
    machine.fail("grep -q 'menu-login' /tmp/pub")
    for secret in ["vm-watched", "vm-tag", "vm-mail@example.org", "vm-dummy-token", "vm-session", "$(cut -d/ -f3 /tmp/feed.path)", "$(cat /tmp/upload.token)"]:
        machine.fail(f"grep -q -e \"{secret}\" /tmp/pub /tmp/pub.json")
    machine.wait_until_succeeds("journalctl -u maintainer-mode -o cat > /tmp/log && grep -q 'GitHub App 1 syncs first' /tmp/log")
    # the key parsed and the JWT was signed; only the request found no network
    machine.wait_until_succeeds("journalctl -u maintainer-mode -o cat > /tmp/log && grep -q 'GitHub App token: error sending request' /tmp/log")
    machine.wait_until_succeeds("journalctl -u maintainer-mode -o cat > /tmp/log && grep -q 'next pass in 10 s' /tmp/log")
    # a new secret reaches the running service through a reload, not a
    # restart, and the value never reaches the log
    machine.succeed("systemctl is-active maintainer-mode-credentials.timer")
    machine.succeed("systemctl start maintainer-mode-credentials.service")
    machine.succeed("journalctl -u maintainer-mode-credentials -o cat > /tmp/cred.log && ! grep -q changed /tmp/cred.log")
    pid = machine.succeed("systemctl show -p MainPID --value maintainer-mode.service").strip()
    machine.succeed("mkdir -p /run/secrets.d/2 && echo new-token > /run/secrets.d/2/bot-token && ln -s /run/secrets.d/2 /run/secrets.tmp && mv -T /run/secrets.tmp /run/secrets && rm -r /run/secrets.d/1")
    # LoadCredential copied the file at start; the copy does not follow
    machine.succeed(f"nsenter -t {pid} -m cat /run/credentials/maintainer-mode.service/bot_token > /tmp/cred && grep -qx old-token /tmp/cred")
    machine.succeed("systemctl start maintainer-mode-credentials.service")
    machine.succeed("journalctl -u maintainer-mode-credentials -o cat > /tmp/cred.log && grep -q 'credential bot_token changed' /tmp/cred.log")
    machine.succeed(f"nsenter -t {pid} -m cat /run/credentials/maintainer-mode.service/bot_token > /tmp/cred && grep -qx new-token /tmp/cred")
    machine.wait_until_succeeds("journalctl -u maintainer-mode -o cat > /tmp/log && grep -q 'bot_token changed; the new secret is in use' /tmp/log")
    machine.succeed(f"systemctl show -p MainPID --value maintainer-mode.service > /tmp/pid && grep -qx {pid} /tmp/pid")
    machine.fail("journalctl -o cat > /tmp/all.log && grep -q -e new-token -e old-token /tmp/all.log")
    # with an index, the public list answers and the server takes only a
    # strict report
    machine.succeed("systemctl stop maintainer-mode.service")
    machine.succeed("cd /var/lib/maintainer-mode && sqlite3 maintainer-mode.sqlite < /etc/seed.sql && chown $(stat -c %u:%g .) maintainer-mode.sqlite*")
    machine.succeed("systemctl start maintainer-mode.service")
    machine.wait_for_open_port(8790)
    machine.succeed("curl -sf -D /tmp/pi.h -o /tmp/pi.json http://127.0.0.1:8790/api/v1/packages/index && grep -qi '^etag: \"1111111111111111111111111111111111111111\"' /tmp/pi.h && grep -qi '^cache-control: public' /tmp/pi.h")
    machine.succeed("grep -q '\"python3Packages.requests\":\\[\"requests\",\"2.31\"\\]' /tmp/pi.json && grep -q '\"requests\":\\[\"python3Packages.requests\"\\]' /tmp/pi.json")
    machine.succeed("curl -sf -H 'Accept-Encoding: gzip' -D /tmp/pigz.h -o /tmp/pi.gz http://127.0.0.1:8790/api/v1/packages/index && grep -qi '^content-encoding: gzip' /tmp/pigz.h && gzip -dc /tmp/pi.gz > /tmp/pi2.json && cmp /tmp/pi.json /tmp/pi2.json")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' -H 'If-None-Match: \"1111111111111111111111111111111111111111\"' http://127.0.0.1:8790/api/v1/packages/index > /tmp/pi.code && grep -q 304 /tmp/pi.code")
    machine.succeed("cd /tmp/scan && MAINTAINER_MODE_UPLOAD_TOKEN=vmtoken maintainer-mode submit --to http://127.0.0.1:8790 > /tmp/submit.json && grep -q '\"kept\":2' /tmp/submit.json && grep -q '\"dropped\":2' /tmp/submit.json && grep -q '\"name\":\"default\"' /tmp/submit.json")
    # a named set, and a public flake set from the owner's own scan
    machine.fail("cd /tmp/scan && MAINTAINER_MODE_UPLOAD_TOKEN=vmtoken maintainer-mode submit --to http://127.0.0.1:8790 --name site --public-flake https://127.0.0.1/x.tar.gz#x")
    machine.succeed("cd /tmp/scan && MAINTAINER_MODE_UPLOAD_TOKEN=vmtoken maintainer-mode submit --to http://127.0.0.1:8790 --name site --public-flake github:o/r#packages.x86_64-linux.default > /tmp/submit2.json && grep -q '\"public\":true' /tmp/submit2.json")
    machine.succeed("curl -sf 'http://127.0.0.1:8790/u/alice?tab=flakes' -o /tmp/u-flakes && grep -q 'github:o/r' /tmp/u-flakes && ! grep -q '>default<' /tmp/u-flakes")
    # a set with output labels, sent with --with-outputs
    machine.succeed("cd /tmp/scan && MAINTAINER_MODE_UPLOAD_TOKEN=vmtoken maintainer-mode submit --to http://127.0.0.1:8790 --report multi.json --name infra --with-outputs > /tmp/submit3.json && grep -q '\"outputs\":2' /tmp/submit3.json")
    machine.succeed("curl -s -o /tmp/r22 -w '%{http_code}' -X POST -H 'Authorization: Bearer vmtoken' -d '{\"schema\":1,\"name\":\"bad\",\"outputs\":[\"a b\"],\"packages\":[]}' http://127.0.0.1:8790/api/v1/reports > /tmp/r22.code && grep -q 400 /tmp/r22.code && grep -q 'output label' /tmp/r22")
    # a follow-up plan on a pull request of alice: only she sees the form
    signed_in("/pr/575334", "/tmp/s-pr")
    machine.succeed("grep -q 'Your follow-up plan' /tmp/s-pr && grep -q 'action=\"/me/followup\"' /tmp/s-pr && grep -q 'Your default plan applies' /tmp/s-pr")
    machine.succeed("curl -sf -o /tmp/pub-pr http://127.0.0.1:8790/pr/575334 && ! grep -q 'Your follow-up plan' /tmp/pub-pr")
    form("/me/followup", "/tmp/f-plan", "-d number=575334 -d days=5 -d action=merge")
    machine.succeed("grep -qi '^location: /pr/575334#followup' /tmp/f-plan.h")
    signed_in("/pr/575334", "/tmp/s-pr2")
    machine.succeed("grep -q 'you get a reminder to merge it yourself.' /tmp/s-pr2 && grep -q 'option value=\"merge\" selected' /tmp/s-pr2 && grep -q 'Use my default' /tmp/s-pr2")
    form("/me/followup", "/tmp/f-plan2", "-d number=575334 -d days=500 -d action=merge", 400)
    signed_in("/api/v1/needs", "/tmp/s-needs2.json")
    machine.succeed("grep -q '\"source\":\"pull_request\"' /tmp/s-needs2.json && grep -q '\"number\":575334' /tmp/s-needs2.json")
    form("/me/followup", "/tmp/f-plan3", "-d number=575334 -d reset=1")
    signed_in("/pr/575334", "/tmp/s-pr3")
    machine.succeed("grep -q 'Your default plan applies' /tmp/s-pr3")
    # with a machine report, the machines filter comes back in the bar, and
    # its packages lead the list
    signed_in("/packages", "/tmp/s-pk-mine2")
    machine.succeed("grep -q 'href=\"/packages?filter=machines\">On my machines <span class=\"Counter\">2</span>' /tmp/s-pk-mine2 && ! grep -q 'Upload the packages of your machines' /tmp/s-pk-mine2")
    machine.succeed("grep -q 'href=\"/p/hello\"' /tmp/s-pk-mine2 && grep -q 'on your machines' /tmp/s-pk-mine2")
    signed_in("/packages?filter=machines", "/tmp/s-pk-machines2")
    machine.succeed("grep -q 'Your sets' /tmp/s-pk-machines2 && grep -q 'set=site' /tmp/s-pk-machines2 && grep -q 'action=\"/me/sets/delete\"' /tmp/s-pk-machines2 && grep -q 'id=\"flakes\"' /tmp/s-pk-machines2")
    machine.succeed("curl -sf -o /tmp/pub2 http://127.0.0.1:8790/packages && ! grep -q -e 'on your machines' -e 'filter=' /tmp/pub2")
    # the same views in the private API: the session, or the MCP token
    signed_in("/api/v1/mine?filter=machines&set=site", "/tmp/s-mine.json")
    machine.succeed("grep -q '\"filter\":\"machines\"' /tmp/s-mine.json && grep -q '\"set\":\"site\"' /tmp/s-mine.json && grep -q '\"why\":\\[\"machine\"' /tmp/s-mine.json")
    signed_in("/api/v1/mine?filter=nope", "/tmp/s-mine-bad.json", 400)
    machine.succeed("grep -q 'use one of mine, machines, maintain, watch' /tmp/s-mine-bad.json")
    signed_in("/api/v1/sets", "/tmp/s-sets.json")
    machine.succeed("grep -q '\"name\":\"default\"' /tmp/s-sets.json && grep -q '\"name\":\"site\"' /tmp/s-sets.json && grep -q '\"reference\":\"github:o/r\"' /tmp/s-sets.json")
    signed_in("/api/v1/inbox?kind=advisory", "/tmp/s-inbox.json")
    machine.succeed("grep -q '\"unread\":' /tmp/s-inbox.json && grep -q '\"login\":\"alice\"' /tmp/s-inbox.json")
    signed_in("/api/v1/progress", "/tmp/s-progress.json")
    machine.succeed("grep -q '\"goal\":4' /tmp/s-progress.json")
    for path in ["/api/v1/mine", "/api/v1/sets", "/api/v1/inbox", "/api/v1/progress"]:
        machine.succeed(f"curl -s -D /tmp/priv0.h -o /dev/null {base}{path} && grep -q '^HTTP/1.1 401' /tmp/priv0.h && grep -qi '^cache-control: private, no-store' /tmp/priv0.h")
        machine.succeed(f"curl -s -o /dev/null -w '%{{http_code}}' -H 'Authorization: Bearer nope' {base}{path} > /tmp/priv1.code && grep -qx 401 /tmp/priv1.code")
    form("/me/mcp-token", "/tmp/f-mcp", code=200)
    machine.succeed("grep -o 'value=\"[0-9a-f]\\{64\\}\" aria-label=\"MCP token\"' /tmp/f-mcp | cut -d'\"' -f2 > /tmp/mcp.token && test -s /tmp/mcp.token")
    machine.succeed(f"curl -sf -D /tmp/bearer.h -o /tmp/bearer.json -H \"Authorization: Bearer $(cat /tmp/mcp.token)\" {base}/api/v1/sets && grep -qi '^cache-control: private, no-store' /tmp/bearer.h && cmp /tmp/bearer.json /tmp/s-sets.json")
    machine.succeed(f"MAINTAINER_MODE_MCP_TOKEN=$(cat /tmp/mcp.token) maintainer-mode --server {base} mine --filter machines --set site > /tmp/cli-mine && grep -q 'machine' /tmp/cli-mine")
    machine.succeed(f"MAINTAINER_MODE_MCP_TOKEN=$(cat /tmp/mcp.token) maintainer-mode --server {base} get /api/v1/progress > /tmp/cli-progress && grep -q '\"goal\":4' /tmp/cli-progress")
    machine.fail(f"maintainer-mode --server {base} get /api/v1/sets")
    # the MCP tool and the endpoint give the same JSON
    machine.succeed(f"curl -sf -o /tmp/mcp-sets.json -H \"Authorization: Bearer $(cat /tmp/mcp.token)\" -H 'Content-Type: application/json' -d '{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{{\"name\":\"my_sets\",\"arguments\":{{}}}}}}' {base}/mcp && grep -q 'github:o/r' /tmp/mcp-sets.json")
    machine.succeed(f"curl -sf -o /tmp/mcp-list.json -H \"Authorization: Bearer $(cat /tmp/mcp.token)\" -H 'Content-Type: application/json' -d '{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}}' {base}/mcp && grep -q '\"name\":\"my_packages\"' /tmp/mcp-list.json && grep -q '\"name\":\"first_packages\"' /tmp/mcp-list.json")
    # the public views in the API
    machine.succeed(f"curl -sf -o /tmp/search.json '{base}/api/v1/search?q=hello' && grep -q '\"key\":\"pkg:hello\"' /tmp/search.json")
    machine.succeed(f"curl -s -o /tmp/search0.json -w '%{{http_code}}' {base}/api/v1/search > /tmp/search0.code && grep -qx 400 /tmp/search0.code && grep -q 'q is missing' /tmp/search0.json")
    machine.succeed(f"curl -sf -o /tmp/first.json {base}/api/v1/first && grep -q '\"total\":' /tmp/first.json")
    machine.succeed(f"curl -sf -o /tmp/help.json {base}/api/v1/need-help && grep -q '\"total\":' /tmp/help.json")
    machine.succeed(f"curl -sf -o /tmp/index.json {base}/api/v1 && grep -q '\"page\":\"/packages?filter=machines&set=\"' /tmp/index.json && grep -q '\"mcp_tool\":\"my_sets\"' /tmp/index.json")
    machine.succeed(f"maintainer-mode --server {base} search hello > /tmp/cli-search && grep -q 'pkg:hello' /tmp/cli-search")
    # each row of the set says where it runs; output= keeps one output
    signed_in("/packages?filter=machines&set=infra", "/tmp/s-infra")
    machine.succeed("grep -q 'Where it runs' /tmp/s-infra && grep -q 'set=infra&amp;output=other\">other</a>' /tmp/s-infra")
    signed_in("/packages?filter=machines&set=infra&output=other", "/tmp/s-infra-other")
    machine.succeed("grep -q 'Packages of infra on other' /tmp/s-infra-other && grep -q 'href=\"/p/hello\"' /tmp/s-infra-other && ! grep -q 'href=\"/p/[^\"]*requests\"' /tmp/s-infra-other")
    signed_in("/packages?filter=machines&set=infra&output=top", "/tmp/s-infra-top")
    machine.succeed("grep -q 'href=\"/p/[^\"]*requests\"' /tmp/s-infra-top")
    signed_in("/api/v1/sets?set=infra&output=other", "/tmp/s-sets-infra.json")
    machine.succeed("grep -q '\"packages\":\\[\"hello\"\\]' /tmp/s-sets-infra.json && grep -q '\"outputs\":\\[\"top\",\"other\"\\]' /tmp/s-sets-infra.json && grep -q '\"source\":\"upload\"' /tmp/s-sets-infra.json")
    signed_in("/api/v1/sets?set=infra&output=nope", "/tmp/s-sets-nope.json", 404)
    signed_in("/api/v1/sets?output=other", "/tmp/s-sets-noset.json", 404)
    signed_in("/api/v1/mine?filter=machines&set=infra&output=other", "/tmp/s-mine-other.json")
    machine.succeed("grep -q '\"output\":\"other\"' /tmp/s-mine-other.json && grep -q '\"key\":\"pkg:hello\"' /tmp/s-mine-other.json && grep -q '{\"output\":\"other\",\"set\":\"infra\"}' /tmp/s-mine-other.json && ! grep -q 'requests' /tmp/s-mine-other.json")
    machine.succeed(f"curl -sf -o /tmp/mcp-infra.json -H \"Authorization: Bearer $(cat /tmp/mcp.token)\" -H 'Content-Type: application/json' -d '{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{{\"name\":\"my_sets\",\"arguments\":{{\"set\":\"infra\",\"output\":\"other\"}}}}}}' {base}/mcp && grep -q 'package_outputs' /tmp/mcp-infra.json && ! grep -q 'isError\":true' /tmp/mcp-infra.json")
    machine.succeed("curl -s -o /tmp/r20 -w '%{http_code}' -X POST -H 'Authorization: Bearer vmtoken' -d '{\"schema\":1,\"packages\":[{\"attr\":\"hello\",\"pname\":\"hello\",\"version\":\"1.2.3-acme-fix\"}]}' http://127.0.0.1:8790/api/v1/reports > /tmp/r20.code && grep -q 400 /tmp/r20.code && grep -q 'does not have version 1.2.3-acme-fix' /tmp/r20")
    machine.succeed("curl -s -o /tmp/r21 -w '%{http_code}' -X POST -H 'Authorization: Bearer vmtoken' -d '{\"schema\":1,\"source\":{\"name\":\"/home/me/tree\"},\"packages\":[]}' http://127.0.0.1:8790/api/v1/reports > /tmp/r21.code && grep -q 400 /tmp/r21.code && grep -q 'unknown field' /tmp/r21")
  '';
}
