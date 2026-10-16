{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.maintainer-mode;
  state = "/var/lib/maintainer-mode";
in
{
  imports = [
    (lib.mkRemovedOptionModule [
      "services"
      "maintainer-mode"
      "nixfreshUrl"
    ] "Maintainer Mode now collects package health itself.")
  ];

  options.services.maintainer-mode = {
    enable = lib.mkEnableOption "Maintainer Mode, the nixpkgs review and merge queues";

    package = lib.mkOption {
      type = lib.types.package;
      description = "The maintainer-mode package. Its wrapper already names the evaluator.";
    };

    listen = lib.mkOption {
      type = lib.types.str;
      default = "127.0.0.1:8790";
    };

    domain = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "maintainer-mode.example.org";
      description = "When set, nginx serves this name with ACME and proxies to `listen`.";
    };

    publicUrl = lib.mkOption {
      type = lib.types.str;
      default = if cfg.domain == null then "http://${cfg.listen}" else "https://${cfg.domain}";
      defaultText = lib.literalExpression ''"https://''${domain}"'';
      description = "The URL people reach. The OAuth callback is `<publicUrl>/auth/callback`.";
    };

    githubClientId = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Client id of the GitHub OAuth app. Without it, login is off and the sync waits for a sync token.";
    };

    githubClientSecretFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "File holding the OAuth client secret, read through LoadCredential; never copied to the store.";
    };

    syncTokenFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "Optional GitHub token owned by the deployment, used before tokens lent by logged-in people. A GitHub App replaces it.";
    };

    nvdApiKeyFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "Optional file holding an NVD API key, read through LoadCredential. Without it, NVD is not asked.";
    };

    githubAppId = lib.mkOption {
      type = lib.types.nullOr lib.types.int;
      default = null;
      description = "Id of a GitHub App. Its installation token is the first sync token, renewed before it expires.";
    };

    githubAppKeyFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "File holding the PEM private key of the GitHub App, read through LoadCredential; never copied to the store.";
    };

    githubAppInstallationId = lib.mkOption {
      type = lib.types.nullOr lib.types.int;
      default = null;
      description = "Installation of the GitHub App to use. Without it, the service uses the installation on the account that owns the App.";
    };

    historySince = lib.mkOption {
      type = lib.types.str;
      default = "2020-01-01";
      description = "How far back the history walk for `/state` goes, as YYYY-MM-DD.";
    };

    indexSets = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [
        "pkgs"
        "python3Packages"
      ];
      description = "Package sets the daily sweep evaluates; `pkgs` is the top level.";
    };

    channels = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [
        "nixos-unstable"
        "stable"
      ];
      example = [
        "nixos-unstable"
        "nixos-26.05"
      ];
      description = "nixpkgs channels indexed next to master. `stable` is the newest stable channel.";
    };

    archive = {
      repositories = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ "NixOS/nixpkgs" ];
        example = [
          "NixOS/nixpkgs"
          "NixOS/nix"
        ];
        description = "Public repositories, as `owner/name`, whose issues and pull requests the archive keeps in full. The walk refuses a private repository, and the API does not serve it. An empty list turns the archive off.";
      };

      itemsPerPass = lib.mkOption {
        type = lib.types.ints.positive;
        default = 300;
        description = "Items the archive walk fetches per repository in each pass. A pass starts every 5 minutes.";
      };
    };

    rescan = {
      enable = lib.mkEnableOption ''
        server-side scans of the public flakes that people register on
        `/me/sets`. This evaluates third-party code in a bubblewrap sandbox;
        read docs/rescan.md first'';

      maxFlakes = lib.mkOption {
        type = lib.types.ints.positive;
        default = 3;
        description = "Flake references a person can register.";
      };

      scansPerDay = lib.mkOption {
        type = lib.types.ints.positive;
        default = 4;
        description = "Scans a person gets in 24 hours, all flakes together.";
      };

      timeout = lib.mkOption {
        type = lib.types.ints.positive;
        default = 900;
        description = "Wall-clock limit of one scan in seconds, fetch and evaluation together.";
      };

      memoryMB = lib.mkOption {
        type = lib.types.ints.positive;
        default = 3072;
        description = "Address-space limit of each sandboxed nix process, in MiB. The service memory limit grows by this much.";
      };
    };

    eventCopy = {
      enable = lib.mkEnableOption "the daily copy of the nixpkgs event log to an S3-compatible store";

      endpoint = lib.mkOption {
        type = lib.types.str;
        example = "https://storage.example.org";
        description = "The S3 endpoint, such as the S3 endpoint of a bunny.net Storage zone.";
      };

      bucket = lib.mkOption {
        type = lib.types.str;
        description = "The bucket. Objects are `YYYY/MM/DD.ndjson.zst` and `YYYY/MM.sqlite.zst`.";
      };

      region = lib.mkOption {
        type = lib.types.str;
        description = "The region that the signature names.";
      };

      credentialsFile = lib.mkOption {
        type = lib.types.path;
        description = ''
          File with `AWS_ACCESS_KEY_ID=…` and `AWS_SECRET_ACCESS_KEY=…` lines,
          read through LoadCredential; never copied to the store.
        '';
      };
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = (cfg.githubClientId == null) == (cfg.githubClientSecretFile == null);
        message = "services.maintainer-mode: set githubClientId and githubClientSecretFile together.";
      }
      {
        assertion = (cfg.githubAppId == null) == (cfg.githubAppKeyFile == null);
        message = "services.maintainer-mode: set githubAppId and githubAppKeyFile together.";
      }
    ];

    systemd.services.maintainer-mode = {
      description = "Maintainer Mode";
      wantedBy = [ "multi-user.target" ];
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ];
      environment = {
        MAINTAINER_MODE_LISTEN = cfg.listen;
        MAINTAINER_MODE_DB = "${state}/maintainer-mode.sqlite";
        MAINTAINER_MODE_PUBLIC_URL = cfg.publicUrl;
        MAINTAINER_MODE_INDEX_SETS = lib.concatStringsSep "," cfg.indexSets;
        MAINTAINER_MODE_CHANNELS = lib.concatStringsSep "," cfg.channels;
        MAINTAINER_MODE_HISTORY_SINCE = cfg.historySince;
        MAINTAINER_MODE_ARCHIVE_REPOS = lib.concatStringsSep "," cfg.archive.repositories;
        MAINTAINER_MODE_ARCHIVE_ITEMS = toString cfg.archive.itemsPerPass;
        HOME = state;
      }
      // lib.optionalAttrs (cfg.githubClientId != null) {
        GITHUB_CLIENT_ID = cfg.githubClientId;
        GITHUB_CLIENT_SECRET_FILE = "%d/client_secret";
      }
      // lib.optionalAttrs (cfg.syncTokenFile != null) {
        MAINTAINER_MODE_SYNC_TOKEN_FILE = "%d/sync_token";
      }
      // lib.optionalAttrs (cfg.nvdApiKeyFile != null) {
        MAINTAINER_MODE_NVD_API_KEY_FILE = "%d/nvd_api_key";
      }
      // lib.optionalAttrs (cfg.githubAppId != null) {
        GITHUB_APP_ID = toString cfg.githubAppId;
        GITHUB_APP_KEY_FILE = "%d/github_app_key";
      }
      // lib.optionalAttrs (cfg.githubAppInstallationId != null) {
        GITHUB_APP_INSTALLATION_ID = toString cfg.githubAppInstallationId;
      }
      // lib.optionalAttrs cfg.eventCopy.enable {
        MAINTAINER_MODE_S3_ENDPOINT = cfg.eventCopy.endpoint;
        MAINTAINER_MODE_S3_BUCKET = cfg.eventCopy.bucket;
        MAINTAINER_MODE_S3_REGION = cfg.eventCopy.region;
        MAINTAINER_MODE_S3_CREDENTIALS_FILE = "%d/s3_credentials";
      }
      // lib.optionalAttrs cfg.rescan.enable {
        MAINTAINER_MODE_RESCAN = "true";
        MAINTAINER_MODE_RESCAN_BWRAP = lib.getExe pkgs.bubblewrap;
        MAINTAINER_MODE_RESCAN_PRLIMIT = "${pkgs.util-linux.bin}/bin/prlimit";
        MAINTAINER_MODE_RESCAN_NIX = "${config.nix.package}/bin/nix";
        MAINTAINER_MODE_RESCAN_MAX_FLAKES = toString cfg.rescan.maxFlakes;
        MAINTAINER_MODE_RESCAN_PER_DAY = toString cfg.rescan.scansPerDay;
        MAINTAINER_MODE_RESCAN_TIMEOUT = toString cfg.rescan.timeout;
        MAINTAINER_MODE_RESCAN_MEMORY_MB = toString cfg.rescan.memoryMB;
      };
      # git fetches the git inputs of a scanned flake, inside the sandbox
      path = lib.optional cfg.rescan.enable pkgs.git;
      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package} serve";
        DynamicUser = true;
        StateDirectory = "maintainer-mode";
        WorkingDirectory = state;
        LoadCredential =
          lib.optional (cfg.githubClientSecretFile != null) "client_secret:${cfg.githubClientSecretFile}"
          ++ lib.optional (cfg.syncTokenFile != null) "sync_token:${cfg.syncTokenFile}"
          ++ lib.optional (cfg.nvdApiKeyFile != null) "nvd_api_key:${cfg.nvdApiKeyFile}"
          ++ lib.optional (cfg.githubAppKeyFile != null) "github_app_key:${cfg.githubAppKeyFile}"
          ++ lib.optional cfg.eventCopy.enable "s3_credentials:${cfg.eventCopy.credentialsFile}";
        Restart = "on-failure";
        # the daily sweep of nixpkgs peaks near 2 GB
        MemoryMax = if cfg.rescan.enable then "${toString (4096 + cfg.rescan.memoryMB)}M" else "4G";
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        NoNewPrivileges = true;
        # bubblewrap cannot mount a new /proc over a /proc with read-only
        # submounts, and sets up the loopback of its network namespace
        # over netlink
        ProtectKernelTunables = !cfg.rescan.enable;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_UNIX"
        ]
        ++ lib.optional cfg.rescan.enable "AF_NETLINK";
        LockPersonality = true;
      };
    };

    services.nginx = lib.mkIf (cfg.domain != null) {
      enable = true;
      recommendedProxySettings = true;
      recommendedTlsSettings = true;
      recommendedGzipSettings = true;
      virtualHosts.${cfg.domain} = {
        enableACME = true;
        forceSSL = true;
        locations."/".proxyPass = "http://${cfg.listen}";
      };
    };
  };
}
