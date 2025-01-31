inputs:

{ config
, lib
, pkgs
, ...
}:

let
  inherit (lib) types;

  cfg = config.services.grapevine;
  configFile = format.generate "config.toml" cfg.settings;
  format = pkgs.formats.toml {};
in

{
  options.services.grapevine = {
    enable = lib.mkEnableOption "grapevine";
    package = lib.mkPackageOption
      inputs.self.packages.${pkgs.system}
      "grapevine"
      {
        default = "default";
        pkgsText = "inputs.grapevine.packages.\${pkgs.system}";
      };

    settings = lib.mkOption {
      type = types.submodule {
        freeformType = format.type;
        options = {
          conduit_compat = lib.mkOption {
            type = types.bool;
            description = ''
              Whether to operate as a drop-in replacement for Conduit.
            '';
            default = false;
          };
          database.path = lib.mkOption {
            type = types.nonEmptyStr;
            readOnly = true;
            description = ''
              The path to store persistent data in.

              Note that this is read-only because this module makes use of
              systemd's `StateDirectory` option.
            '';
            default = if cfg.settings.conduit_compat
              then "/var/lib/matrix-conduit"
              else "/var/lib/grapevine";
          };
          listen = lib.mkOption {
            type = types.listOf format.type;
            description = ''
              List of places to listen for incoming connections.
            '';
            default = [
              {
                type = "tcp";
                address = "::1";
                port = 6167;
              }
            ];
          };
        };
      };
      default = {};
      description = ''
        The TOML configuration file is generated from this attribute set.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.grapevine = {
      description = "Grapevine (Matrix homeserver)";
      wantedBy = [ "multi-user.target" ];

      # Keep sorted
      serviceConfig = {
        DynamicUser = true;
        ExecStart = "${lib.getExe cfg.package} --config ${configFile}";
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        PrivateDevices = true;
        PrivateMounts = true;
        PrivateUsers = true;
        ProtectClock = true;
        ProtectControlGroups = true;
        ProtectHostname = true;
        ProtectKernelLogs = true;
        ProtectKernelModules = true;
        ProtectKernelTunables = true;
        Restart = "on-failure";
        RestartSec = 10;
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        StartLimitBurst = 5;
        StateDirectory = if cfg.settings.conduit_compat
          then "matrix-conduit"
          else "grapevine";
        StateDirectoryMode = "0700";
        SystemCallArchitectures = "native";
        SystemCallFilter = [ "@system-service" "~@privileged" ];
        UMask = "077";
        User = if cfg.settings.conduit_compat
          then "conduit"
          else "grapevine";
      };
    };
  };
}
