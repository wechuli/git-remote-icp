{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    flake-utils.url = "github:numtide/flake-utils";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs = {
        nixpkgs.follows = "nixpkgs";
        flake-utils.follows = "flake-utils";
      };
    };
  };

  outputs = {
    self,
    nixpkgs,
    crane,
    flake-utils,
    rust-overlay
  }:
    let
      supportedSystems = [
        flake-utils.lib.system.aarch64-darwin
        flake-utils.lib.system.x86_64-darwin
      ];
    in
      flake-utils.lib.eachSystem supportedSystems (system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [
              (import rust-overlay)
            ];
          };

          # rust = pkgs.rust-bin.stable.latest.default;
          rust = pkgs.rust-bin.nightly."2022-10-31".default;

          # NB: we don't need to overlay our custom toolchain for the *entire*
          # pkgs (which would require rebuilding anything else which uses rust).
          # Instead, we just want to update the scope that crane will use by appending
          # our specific toolchain there.
          craneLib = (crane.mkLib pkgs).overrideToolchain rust;
          # craneLib = crane.lib."${system}";

          src = ./.;

          hyper-cgi = craneLib.buildPackage rec {
            src = pkgs.fetchFromGitHub {
              owner = "josh-project";
              repo = "josh";
              rev = "b93518b172d656820744dbee605964811fd6cd99";
              sha256 = "sha256-PowrE8iTyR/47I6JcGqqeqeVaU517toHu891YnWVQRs=";
            };
            cargoExtraArgs = "--package hyper_cgi --bin hyper-cgi-test-server --features hyper_cgi/test-server";
          };

          git-remote-http-reqwest = pkgs.callPackage ./nix/git-remote-helper.nix rec {
            inherit craneLib src;
            scheme = { internal = "http"; external = "http-reqwest"; };
            port = 8888;
            installCheckInputs = [
              hyper-cgi
              pkgs.git
            ];
            configure = ''
              git config --global --type bool http.receivePack true
            '';
            setup = ''
              # Start HTTP server

              GIT_DIR=./ GIT_PROJECT_ROOT=./ GIT_HTTP_EXPORT_ALL=1 hyper-cgi-test-server \
                --port ${port} \
                --dir=./test-repo-bare/ \
                --cmd=git \
                --args=http-backend &

              HTTP_SERVER_PID=$!

              trap "EXIT_CODE=\$? && kill \$HTTP_SERVER_PID && exit \$EXIT_CODE" EXIT
            '';
            teardown = ''
              # Exit cleanly
              kill "$HTTP_SERVER_PID"
            '';
          };

          git-remote-icp = pkgs.callPackage ./nix/git-remote-helper.nix {
            inherit craneLib src;
            scheme = { internal = "http"; external = "icp"; };
            configure = ''
              git config --global --type bool icp.fetchRootKey true
              git config --global icp.replicaUrl http://localhost:8000
              git config --global icp.canisterId rwlgt-iiaaa-aaaaa-aaaaa-cai
              git config --global icp.privateKey "$PWD/identity.pem"
            '';
            setup = ''
              exit 1
            '';
            teardown = ''
              exit 1
            '';
          };

          git-remote-tcp = pkgs.callPackage ./nix/git-remote-helper.nix rec {
            inherit craneLib src;
            scheme = { internal = "git"; external = "tcp"; };
            # DEFAULT_GIT_PORT is 9418
            port = 9418;
            setup = ''
              # Start Git daemon

              # Based on https://github.com/Byron/gitoxide/blob/0c9c48b3b91a1396eb1796f288a2cb10380d1f14/tests/helpers.sh#L59
              git daemon --verbose --base-path=test-repo-bare --enable=receive-pack --export-all &
              GIT_DAEMON_PID=$!

              trap "EXIT_CODE=\$? && kill \$GIT_DAEMON_PID && exit \$EXIT_CODE" EXIT
            '';
            teardown = ''
              # Exit cleanly
              kill "$GIT_DAEMON_PID"
            '';
          };

          apps = {
            hyper-cgi = flake-utils.lib.mkApp {
              drv = hyper-cgi;
            };

            git-remote-tcp = flake-utils.lib.mkApp {
              drv = git-remote-tcp;
            };
          };
        in
          rec {
            checks = {
              inherit
                hyper-cgi
                git-remote-http-reqwest
                # git-remote-icp
                git-remote-tcp
              ;
            };

            packages = {
              inherit
                hyper-cgi
                git-remote-http-reqwest
                # git-remote-icp
                git-remote-tcp
              ;
            };

            inherit apps;

            # defaultPackage = packages.git-remote-icp;
            # defaultApp = apps.git-remote-icp;

            devShell = pkgs.mkShell {
              # RUST_SRC_PATH = pkgs.rustPlatform.rustLibSrc;
              RUST_SRC_PATH = pkgs.rust.packages.stable.rustPlatform.rustLibSrc;
              inputsFrom = builtins.attrValues checks;
              nativeBuildInputs = pkgs.lib.foldl
                (state: drv: builtins.concatLists [state drv.nativeBuildInputs])
                []
                (pkgs.lib.attrValues packages)
              ;
            };
          }
      );
}
