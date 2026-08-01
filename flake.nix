{
  description = "Rust project";

  inputs = {
    rs-harbor = {
      url = "git+https://codeberg.org/caniko/rs-harbor.git?ref=trunk&rev=b40cd4c4fdf6133962f67bd68a48bfd5d554d47f";
      inputs.nix-opencode-lsp.url = "github:caniko/nix-opencode-lsp";
    };
    nixpkgs.follows = "rs-harbor/nixpkgs";
    rust-overlay.follows = "rs-harbor/rust-overlay";
    crane.follows = "rs-harbor/crane";
    flake-utils.url = "github:numtide/flake-utils";
    treefmt-nix.url = "github:numtide/treefmt-nix";
    git-hooks.url = "github:cachix/git-hooks.nix";
    nix-pklx-src = {
      url = "git+https://codefloe.com/caniko/nix-pklx.git?ref=0.1.0&rev=ed0e5889bcbba74b2e43593954327e3631126908";
      flake = false;
    };
  };

  outputs = {
    self,
    rs-harbor,
    nixpkgs,
    rust-overlay,
    crane,
    flake-utils,
    treefmt-nix,
    git-hooks,
    nix-pklx-src,
    ...
  }:
    flake-utils.lib.eachDefaultSystem (system: let
      lib = nixpkgs.lib;
      pkgs = import nixpkgs {
        inherit system;
        overlays = [(import rust-overlay)];
      };

      toolchain = rs-harbor.lib.mkToolchain {
        inherit pkgs;
        cache.enable = false;
        crossTargets = ["x86_64-unknown-linux-musl"];
      };
      inherit (toolchain) craneLib rustToolchain;
      src = lib.cleanSourceWith {
        src = ./.;
        filter = path: type:
          craneLib.filterCargoSources path type
          || lib.hasSuffix ".pkl" path;
      };
      commonArgs = {
        inherit src;
        strictDeps = true;
      };
      cargoArtifacts = craneLib.buildDepsOnly commonArgs;
      package = craneLib.buildPackage (commonArgs
        // {
          inherit cargoArtifacts;
          nativeBuildInputs = [pkgs.makeWrapper];
          nativeCheckInputs = [pkgs.cacert pkgs.ffmpeg-full];
          SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
          postInstall = ''
            wrapProgram "$out/bin/marchiver" \
              --prefix PATH : ${lib.makeBinPath [pkgs.ffmpeg-full]} \
              --set SSL_CERT_FILE ${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt
          '';
        });
      pklxPackage = pkgs.rustPlatform.buildRustPackage {
        pname = "pklx";
        version = "0.1.0";
        src = nix-pklx-src;
        cargoLock.lockFile = "${nix-pklx-src}/Cargo.lock";
        nativeBuildInputs = [pkgs.makeWrapper];
        nativeCheckInputs = [pkgs.cacert];
        SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
        postInstall = ''
          wrapProgram "$out/bin/pklx" \
            --set SSL_CERT_FILE ${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt
        '';
      };
      pklxLib = import "${nix-pklx-src}/lib" {
        inherit lib pkgs;
        pklx = pklxPackage;
      };
      defaultConfig = pklxLib.importPkl ./pkl/Config.pkl;
      treefmtEval = treefmt-nix.lib.evalModule pkgs (import ./nix/treefmt.nix);
      pre-commit-check = git-hooks.lib.${system}.run {
        src = ./.;
        hooks = import ./nix/pre-commit.nix {
          inherit pkgs;
          treefmtWrapper = treefmtEval.config.build.wrapper;
          inherit rustToolchain;
        };
      };
    in {
      packages.default = package;
      formatter = treefmtEval.config.build.wrapper;
      checks =
        {
          default = package;
          formatting = treefmtEval.config.build.check self;
          clippy = craneLib.cargoClippy (commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets --all-features -- --deny warnings";
            });
          fmt = craneLib.cargoFmt {inherit src;};
          tests = craneLib.cargoTest (commonArgs
            // {
              inherit cargoArtifacts;
              nativeBuildInputs = [pkgs.cacert pkgs.ffmpeg-full];
              SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
              cargoTestExtraArgs = "--all-targets --all-features";
            });
        }
        // {
          pkl-contract = assert defaultConfig.schemaVersion == 1;
            pkgs.runCommand "marchiver-pkl-contract" {
              nativeBuildInputs = [pkgs.pkl pklxPackage];
            } ''
              pkl eval -f json ${./pkl/Config.pkl} > config.json
              pklx eval --data-only ${./pkl/Config.pkl} > config.nix
              test -s config.json
              test -s config.nix
              touch "$out"
            '';
        };
      lib = {inherit defaultConfig;};
      devShells.default = craneLib.devShell {
        checks = self.checks.${system};
        CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER = "${pkgs.pkgsMusl.stdenv.cc}/bin/cc";
        CC_x86_64_unknown_linux_musl = "${pkgs.pkgsMusl.stdenv.cc}/bin/cc";
        packages = with pkgs;
          [
            cargo-about
            cargo-audit
            cargo-cyclonedx
            cargo-deny
            cargo-llvm-cov
            cargo-sbom
            cargo-nextest
            cosign
            file
            ffmpeg-full
            gnutar
            gzip
            jq
            minisign
            nodejs
            pkl
            pkgsMusl.stdenv.cc
            pre-commit
            rpm
            util-linux
            unzip
            zip
            reprepro
            rust-analyzer
            taplo
          ]
          ++ [pklxPackage]
          ++ pre-commit-check.enabledPackages;
        shellHook = pre-commit-check.shellHook;
      };
      apps.local-check-fast = {
        type = "app";
        program = let
          script = pkgs.writeShellApplication {
            name = "local-check-fast";
            runtimeInputs = with pkgs; [
              cargo-deny
              git
              jq
              rustToolchain
            ];
            text = ''
              set -euo pipefail
              cargo test --workspace --all-features
              cargo clippy --workspace --all-targets --all-features -- --deny warnings
              cargo deny check bans licenses sources
              cargo package --workspace --allow-dirty --list >/dev/null
            '';
          };
        in "${script}/bin/local-check-fast";
        meta.description = "Run fast local validation checks";
      };
      apps.local-check-release = {
        type = "app";
        program = let
          script = pkgs.writeShellApplication {
            name = "local-check-release";
            runtimeInputs = with pkgs; [
              cargo-about
              cargo-cyclonedx
              cargo-deny
              cargo-sbom
              cosign
              jq
              minisign
              rustToolchain
            ];
            text = ''
              set -euo pipefail
              version="''${1:-}"
              if [ -z "$version" ]; then
                echo "usage: local-check-release <version>" >&2
                exit 2
              fi
              repo="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
              cd "$repo"
              ${self.apps.${system}.local-check-fast.program}
              manifest="release/artifacts.json"
              mkdir -p release
              jq -n --arg version "$version" \
                '{version: $version, artifacts: [], skipped: [], generated_by: "simit local-check-release"}' \
                > "$manifest.tmp"
              mv "$manifest.tmp" "$manifest"
              manifest_add_file() {
                path="$1"
                producer="$2"
                [ -f "$path" ] || return 0
                sha256="$(sha256sum "$path" | awk '{print $1}')"
                jq --arg path "$path" --arg sha256 "$sha256" --arg producer "$producer" \
                  '.artifacts += [{path: $path, sha256: $sha256, producer: $producer}]' \
                  "$manifest" > "$manifest.tmp"
                mv "$manifest.tmp" "$manifest"
              }
              manifest_skip() {
                name="$1"
                reason="$2"
                jq --arg name "$name" --arg reason "$reason" \
                  '.skipped += [{name: $name, reason: $reason}]' \
                  "$manifest" > "$manifest.tmp"
                mv "$manifest.tmp" "$manifest"
              }
              if [ -f about-template.hbs ]; then
                cargo about generate --output-file release/THIRD_PARTY_LICENSES.html about-template.hbs
                manifest_add_file release/THIRD_PARTY_LICENSES.html cargo-about
              else
                echo "warning: about-template.hbs not found; skipping cargo-about report" >&2
                manifest_skip cargo-about "about-template.hbs not found"
              fi
              cargo sbom --output-format cyclone_dx_json_1_5 > "release/''${version}.cdx.json"
              cargo sbom --output-format spdx_json_2_3 > "release/''${version}.spdx.json"
              manifest_add_file "release/''${version}.cdx.json" cargo-sbom-cyclonedx
              manifest_add_file "release/''${version}.spdx.json" cargo-sbom-spdx
              if [ -n "''${COSIGN_PRIVATE_KEY:-}" ]; then
                echo "COSIGN_PRIVATE_KEY present; local release parity will not sign or upload" >&2
              else
                echo "warning: keyless Sigstore and COSIGN_PRIVATE_KEY unavailable locally; skipping local cosign signing" >&2
                manifest_skip cosign "keyless Sigstore and COSIGN_PRIVATE_KEY unavailable locally"
              fi
              if [ -x scripts/release-local-check.sh ]; then
                bash scripts/release-local-check.sh "$version"
              fi
              echo "local release parity dry-run passed for ''${version}; no external publish was attempted"
            '';
          };
        in "${script}/bin/local-check-release";
        meta.description = "Run local release parity checks without publishing";
      };
      apps.local-release-deploy = {
        type = "app";
        program = let
          script = pkgs.writeShellApplication {
            name = "local-release-deploy";
            runtimeInputs = with pkgs; [
              git
              jq
            ];
            text = ''
              set -euo pipefail
              version="''${1:-}"
              publish_flag="''${2:-}"
              publish_version="''${3:-}"
              if [ -z "$version" ] || [ "$publish_flag" != "--publish" ] || [ "$publish_version" != "$version" ]; then
                echo "usage: local-release-deploy <version> --publish <version>" >&2
                echo "refusing to publish without an explicit matching confirmation" >&2
                exit 2
              fi
              repo="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
              cd "$repo"
              ${self.apps.${system}.local-check-release.program} "$version"
              if ! jq -e --arg version "$version" '.version == $version' release/artifacts.json >/dev/null; then
                echo "release/artifacts.json is missing or does not match version $version" >&2
                exit 1
              fi
              if [ -x scripts/local-release-deploy.sh ]; then
                SIMIT_LOCAL_RELEASE_CHECK_DONE=1 exec bash scripts/local-release-deploy.sh "$version" --publish "$version"
              fi
              echo "local-release-deploy has no project publisher hook at scripts/local-release-deploy.sh" >&2
              echo "Homebrew-capable hooks must build Darwin tarballs and gate tap pushes on HOMEBREW_TAP_TOKEN" >&2
              echo "local-check-release must remain non-publishing: no brew bump, git push, upload, or cargo publish" >&2
              exit 2
            '';
          };
        in "${script}/bin/local-release-deploy";
        meta.description = "Run the guarded local release deployment hook";
      };
    });
}
