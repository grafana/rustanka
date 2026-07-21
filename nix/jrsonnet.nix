{
  lib,
  craneLib,
  makeWrapper,
  withExperimentalFeatures ? false,
  forBenchmarks ? false,

  cpp-jsonnet-for-tests,
  go-jsonnet-for-tests,
}:
let
  inherit (lib) optionalString;
in
craneLib.buildPackage {
  src = lib.cleanSourceWith {
    src = ../.;
    filter =
      path: type:
      (lib.hasSuffix "\.jsonnet" path)
      || (lib.hasSuffix "\.ungram" path)
      || (lib.hasSuffix "\.golden" path)
      || (lib.hasSuffix "\.snap" path)
      || (craneLib.filterCargoSources path type);
  };
  pname = "jrsonnet";
  version = "current${optionalString withExperimentalFeatures "-experimental"}";

  cargoExtraArgs = "--locked --features=mimalloc${optionalString withExperimentalFeatures ",experimental"}";
  cargoTestExtraArgs = "--workspace";

  CPP_JSONNET_FOR_TESTS = cpp-jsonnet-for-tests;
  GO_JSONNET_FOR_TESTS = go-jsonnet-for-tests;

  nativeBuildInputs = [ makeWrapper ];

  # To clean-up hyperfine output
  postInstall = optionalString forBenchmarks ''
    wrapProgram $out/bin/jrsonnet --add-flags "--max-stack=200000"
  '';
}
