# The NixOS module: runs the bot as a systemd service, with its config.toml
# written from `services.assistant-bot.settings` and its secrets from an
# environment file. `self` is this flake, for its package.
self:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.assistant-bot;
  toml = pkgs.formats.toml { };
  configFile = toml.generate "assistant-bot.toml" cfg.settings;

  name = "assistant-bot";
  stateDir = "/var/lib/${name}";

  # The bot's command line with the service's config, environment and user,
  # e.g. `assistant-bot settings list` or `assistant-bot check-config`.
  cli = pkgs.writeShellApplication {
    inherit name;
    runtimeInputs = [ pkgs.util-linux ];
    text = ''
      if [ "$(id -u)" -ne 0 ]; then
        exec sudo "$0" "$@"
      fi
      ${lib.optionalString (cfg.environmentFile != null) ''
        set -a
        # shellcheck disable=SC1091
        . ${lib.escapeShellArg cfg.environmentFile}
        set +a
      ''}
      cd ${stateDir}
      exec runuser -u ${name} -- ${lib.getExe cfg.package} --config ${configFile} "$@"
    '';
  };
in
{
  options.services.assistant-bot = {
    enable = lib.mkEnableOption "the assistant Telegram bot";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "assistant_bot_rs.packages.\${system}.default";
      description = "The bot's package.";
    };

    settings = lib.mkOption {
      inherit (toml) type;
      default = { };
      example = lib.literalExpression ''
        {
          timezone = "Asia/Kolkata";
          telegram.owner_id = 123456789;
          modules.trips.default_currency = "INR";
        }
      '';
      description = ''
        The bot's config.toml: see config.example.toml for every key. It is
        written to the Nix store, which any local user can read, so pass the
        secrets (the bot token, the AI token, the lights' local keys) in
        {option}`services.assistant-bot.environmentFile` instead. The database
        defaults to SQLite in ${stateDir}.
      '';
    };

    environmentFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      example = "/etc/secrets/assistant-bot.env";
      description = ''
        A file of `ASSISTANT_<KEY__PATH>=value` lines, read by the service and
        by the `assistant-bot` command, for the settings that must stay out of
        the Nix store: `ASSISTANT_TELEGRAM__BOT_TOKEN`,
        `ASSISTANT_AI__OAUTH_TOKEN`, and
        `ASSISTANT_MODULES__LIGHTS__DEVICES__<LIGHT>__LOCAL_KEY`.
      '';
    };

    claudePackage = lib.mkOption {
      type = lib.types.nullOr lib.types.package;
      default = null;
      example = lib.literalExpression "pkgs.claude-code";
      description = ''
        The Claude Code CLI that the AI features run (it is unfree). Sets
        `ai.claude_path`; the AI also needs `ASSISTANT_AI__OAUTH_TOKEN`.
      '';
    };

    openLightsFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Whether to open UDP ports 6666, 6667 and 7000, on which Tuya bulbs
        broadcast: the lights module finds a bulb without an `address` there.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    services.assistant-bot.settings = {
      database.url = lib.mkDefault "sqlite://${stateDir}/assistant_bot.sqlite?mode=rwc";
      ai.claude_path = lib.mkIf (cfg.claudePackage != null) (
        lib.mkDefault (lib.getExe cfg.claudePackage)
      );
    };

    users.users.${name} = {
      isSystemUser = true;
      group = name;
      home = stateDir;
    };
    users.groups.${name} = { };

    systemd.services.${name} = {
      description = "The assistant Telegram bot";
      wantedBy = [ "multi-user.target" ];
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ];

      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package} --config ${configFile} run";
        User = name;
        Group = name;
        StateDirectory = name;
        StateDirectoryMode = "0750";
        WorkingDirectory = stateDir;
        EnvironmentFile = lib.mkIf (cfg.environmentFile != null) cfg.environmentFile;
        Restart = "on-failure";
        RestartSec = 10;

        # Hardening. Not MemoryDenyWriteExecute: the Claude CLI's runtime
        # compiles code as it runs.
        CapabilityBoundingSet = "";
        LockPersonality = true;
        NoNewPrivileges = true;
        PrivateDevices = true;
        PrivateTmp = true;
        ProtectClock = true;
        ProtectControlGroups = true;
        ProtectHome = true;
        ProtectHostname = true;
        ProtectKernelLogs = true;
        ProtectKernelModules = true;
        ProtectKernelTunables = true;
        ProtectSystem = "strict";
        RemoveIPC = true;
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_NETLINK"
          "AF_UNIX"
        ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        SystemCallArchitectures = "native";
        UMask = "0077";
      };
    };

    networking.firewall.allowedUDPPorts = lib.mkIf cfg.openLightsFirewall [
      6666
      6667
      7000
    ];

    environment.systemPackages = [ cli ];
  };
}
