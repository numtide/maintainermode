{ config, lib, ... }:
let
  cfg = config.maintainermode;
in
{
  options.maintainermode = {
    enable = lib.mkEnableOption "a public Maintainer Mode instance";

    domain = lib.mkOption {
      type = lib.types.str;
      default = "maintainermode.org";
    };

    githubClientId = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = "Iv23li5X3LR4yyhsc3NS";
      description = ''
        Client id of the GitHub App that people log in with. The default is
        the maintainermode.org app. The app must not expire user tokens,
        because the server keeps no refresh token.
      '';
    };

    githubClientSecretFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
    };

    botTokenFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "The classic token of the bot account, scope `notifications`: it reads the notifications, polls the Events API, and syncs after the GitHub App.";
    };

    githubAppId = lib.mkOption {
      type = lib.types.nullOr lib.types.int;
      default = null;
      description = "Id of the GitHub App whose installation token syncs the instance.";
    };

    githubAppKeyFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
    };

    githubAppInstallationId = lib.mkOption {
      type = lib.types.nullOr lib.types.int;
      default = null;
    };

    probes = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      readOnly = true;
      default = [ "https://${cfg.domain}/healthz" ];
      description = "URLs that answer 200 when the instance is healthy, for the host's monitoring.";
    };
  };

  config = lib.mkIf cfg.enable {
    services.maintainer-mode = {
      enable = true;
      inherit (cfg)
        domain
        botTokenFile
        githubClientSecretFile
        githubAppId
        githubAppKeyFile
        githubAppInstallationId
        ;
      githubClientId = if cfg.githubClientSecretFile == null then null else cfg.githubClientId;
      metrics.listen = "127.0.0.1:9431";
    };
  };
}
