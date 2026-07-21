{
  config,
  lib,
  withSystem,
  ...
}:
let
  inherit (lib)
    mkOption
    mkIf
    types
    concatStringsSep
    ;
  cfg = config.hercules-ci.post-comment;
in
{
  options.hercules-ci.post-comment = {
    enable = mkOption {
      type = types.bool;
      default = false;
      description = ''
        Whether to post a GitHub commit comment for every commit Hercules CI runs on.
      '';
    };
    script = mkOption {
      type = types.lines;
      description = ''
        Bash snippet that writes the comment body to `$out`. Runs as part of the effect
        (after secrets are loaded), so the helpers below are in scope:

        - `nixTar <store-path>` — prints a signed deltarocks URL that streams the path
          as a tar.zst, realised through the configured caches.
        - `nixRender <store-path>` — prints a signed deltarocks URL that renders the
          path's AsciiDoc content as HTML.
      '';
      example = lib.literalExpression ''
        '''
          {
            echo "Render: $(nixRender ''${benchmarks})"
            echo "Tar:    $(nixTar ''${binary})"
          } > $out
        '''
      '';
    };
    system = mkOption {
      type = types.str;
      default = "x86_64-linux";
      description = ''
        System on which the effect runs.
      '';
    };
    baseUrl = mkOption {
      type = types.str;
      default = "https://delta.rocks";
      description = ''
        Base URL of the deltarocks signing service.
      '';
    };
    caches = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "jrsonnet.cachix.org" ];
      description = ''
        Cache hosts the signing service should use as substituters when realising the
        signed store path.
      '';
    };
    signSecret = mkOption {
      type = types.str;
      default = "deltarocks-nix-sign";
      description = ''
        Name of the Hercules CI agent secret that holds the deltarocks signing key.
        Its `data` must have a field named `ogSecret`.
      '';
    };
  };

  config = mkIf cfg.enable {
    herculesCI =
      { config, ... }:
      {
        onPush.default.outputs.effects.post-comment = withSystem cfg.system (
          { pkgs, hci-effects, ... }:
          hci-effects.mkEffect {
            name = "post-comment";
            inputs = [ pkgs.openssl ];
            secretsMap = {
              token = {
                type = "GitToken";
              };
              ogSecret = cfg.signSecret;
            };
            owner = config.repo.owner;
            repoName = config.repo.name;
            rev = config.repo.rev;
            baseUrl = cfg.baseUrl;
            caches = concatStringsSep " " cfg.caches;
            effectScript = ''
              set -euo pipefail

              token=$(readSecretString token .token)
              ogSecret=$(readSecretString ogSecret .ogSecret)
              read -ra cacheArr <<<"$caches"
              if [[ ''${#cacheArr[@]} -eq 0 ]]; then
                echo "hercules-ci.post-comment: at least one cache host is required" >&2
                exit 1
              fi
              sortedCaches=$(printf '%s\n' "''${cacheArr[@]}" | LC_ALL=C sort | paste -sd,)

              _hmacHex() {
                printf '%s' "$1" \
                  | openssl dgst -sha256 -hmac "$ogSecret" -hex \
                  | sed 's/^.*= //'
              }

              _uri() {
                jq -nj --arg s "$1" '$s|@uri'
              }

              _signedUrl() {
                local endpoint=$1 drv=$2
                local sig
                sig=$(_hmacHex "''${endpoint}:''${sortedCaches}:''${drv}")
                local query=""
                for c in "''${cacheArr[@]}"; do
                  query+="cache=$(_uri "$c")&"
                done
                query+="drv=$(_uri "$drv")&sig=''${sig}"
                printf '%s/%s?%s' "$baseUrl" "$endpoint" "$query"
              }

              nixTar() { _signedUrl nixTar "$1"; }
              nixRender() { _signedUrl nixRender "$1"; }

              out=$(mktemp)
              ${cfg.script}

              jq -n --rawfile content "$out" '{body: $content}' \
                | curl -fsSL -X POST \
                    -H "Authorization: Bearer $token" \
                    -H "Accept: application/vnd.github+json" \
                    -H "X-GitHub-Api-Version: 2022-11-28" \
                    --data-binary @- \
                    "https://api.github.com/repos/$owner/$repoName/commits/$rev/comments"
            '';
          }
        );
      };
  };
}
