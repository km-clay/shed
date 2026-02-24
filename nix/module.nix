{ config, lib, pkgs, ... }:

let
  cfg = config.programs.shed;
in
{
  options.programs.shed = {
    enable = lib.mkEnableOption "shed shell";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.shed;
      description = "The shed package to use";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
    environment.shells = [ cfg.package ];
  };
}
