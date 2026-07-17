{ rustfmt }:
{
  settings.global.excludes = [
    "*.jsonnet"
    "*.libsonnet"
  ];

  programs.nixfmt.enable = true;
  programs.rustfmt = {
    enable = true;
    package = rustfmt;
  };
  programs.taplo.enable = true;
}
