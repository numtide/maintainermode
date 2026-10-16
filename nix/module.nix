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
        MAINTAINER_MODE_HISTORY_SINCE = cfg.historySince;
        HOME = state;
      }
      // lib.optionalAttrs (cfg.githubClientId != null) {
        GITHUB_CLIENT_ID = cfg.githubClientId;
        GITHUB_CLIENT_SECRET_FILE = "%d/client_secret";
      }
      // lib.optionalAttrs (cfg.syncTokenFile != null) {
        MAINTAINER_MODE_SYNC_TOKEN_FILE = "%d/sync_token";
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
      };
      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package} serve";
        DynamicUser = true;
        StateDirectory = "maintainer-mode";
        WorkingDirectory = state;
        LoadCredential =
          lib.optional (cfg.githubClientSecretFile != null) "client_secret:${cfg.githubClientSecretFile}"
          ++ lib.optional (cfg.syncTokenFile != null) "sync_token:${cfg.syncTokenFile}"
          ++ lib.optional (cfg.githubAppKeyFile != null) "github_app_key:${cfg.githubAppKeyFile}"
          ++ lib.optional cfg.eventCopy.enable "s3_credentials:${cfg.eventCopy.credentialsFile}";
        Restart = "on-failure";
        # the daily sweep of nixpkgs peaks near 2 GB
        MemoryMax = "4G";
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        NoNewPrivileges = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_UNIX"
        ];
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
