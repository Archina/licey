{
  description = "mslicer — a high-performance MSLA resin slicer";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems =
        f:
        nixpkgs.lib.genAttrs systems (
          system: f system nixpkgs.legacyPackages.${system}
        );

      # winit and wgpu resolve these with dlopen at runtime, so they have to be
      # on LD_LIBRARY_PATH rather than merely linked into the binary.
      runtimeLibraries =
        pkgs: with pkgs; [
          vulkan-loader
          libglvnd
          libxkbcommon
          wayland
          libx11
          libxcursor
          libxi
          libxrandr
        ];
    in
    {
      packages = forAllSystems (
        system: pkgs:
        let
          mslicer = pkgs.rustPlatform.buildRustPackage {
            pname = "mslicer";
            version = "0.9.2";
            src = ./.;

            cargoLock = {
              lockFile = ./Cargo.lock;
              # `soon` comes from a git dependency; `vendor/rustix` is the
              # `[patch.crates-io]` path copy and travels with the source.
              outputHashes = {
                "soon-0.1.0" = "sha256-6hlngcFmhcgdmoDs1RDR0Xy9JSM2SXmhFW4YamQAwpU=";
              };
            };

            nativeBuildInputs = [ pkgs.makeWrapper ];

            doCheck = false;

            postInstall = ''
              wrapProgram $out/bin/mslicer \
                --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath (runtimeLibraries pkgs)}
            '';

            meta = with pkgs.lib; {
              description = "High-performance, open-source slicer for MSLA resin printers";
              homepage = "https://github.com/connorslade/mslicer";
              license = licenses.gpl3Only;
              mainProgram = "mslicer";
              platforms = platforms.linux ++ platforms.darwin;
            };
          };
        in
        {
          default = mslicer;
          inherit mslicer;
        }
      );

      apps = forAllSystems (system: _pkgs: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/mslicer";
        };
      });
    };
}
