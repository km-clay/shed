{ config, lib, pkgs, ... }:

let
  cfg = config.programs.shed;
in
{
  options.programs.shed = import ./shed_opts.nix { inherit pkgs lib; };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
    environment.shells = [ cfg.package ];
    environment.etc."shed/shedrc".text = import ./render_rc.nix lib cfg;
  };
}
