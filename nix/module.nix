{ config, lib, pkgs, ... }:

let
  cfg = config.programs.fern;
in
{
  options.programs.fern = {
    enable = lib.mkEnableOption "fern shell";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.fern;
      description = "The fern package to use";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
    environment.shells = [ cfg.package ];
  };
}
