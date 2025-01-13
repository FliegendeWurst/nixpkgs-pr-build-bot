{
  description = "nixpkgs PR build bot";

  # Nixpkgs / NixOS version to use.
  inputs.nixpkgs.url = "nixpkgs/nixos-24.11";

  outputs = { self, nixpkgs }:
    let

      lib = nixpkgs.lib;

      # to work with older version of flakes
      lastModifiedDate = self.lastModifiedDate or self.lastModified or "19700101";

      # Generate a user-friendly version number.
      version = builtins.substring 0 8 lastModifiedDate;

      # System types to support.
      supportedSystems = [ "x86_64-linux" "x86_64-darwin" "aarch64-linux" "aarch64-darwin" "x86_64-linux-cross-aarch64-linux" ];

      # Helper function to generate an attrset '{ x86_64-linux = f "x86_64-linux"; ... }'.
      forAllSystems = lib.genAttrs supportedSystems;

      # Nixpkgs instantiated for supported system types.
      nixpkgsFor = forAllSystems (system: let parts = lib.splitString "-cross-" system; in (
        if (lib.length parts) == 1 then import nixpkgs { inherit system; } else import nixpkgs {
          localSystem = lib.elemAt parts 0;
          hostSystem = lib.elemAt parts 0;
          crossSystem = lib.elemAt parts 1;
        }
      ));

    in
    {

      # Provide some binary packages for selected system types.
      packages = forAllSystems (system:
        let
          pkgs = nixpkgsFor.${system};
        in
        {
          nixpkgs-pr-build-bot = pkgs.rustPlatform.buildRustPackage rec {
            pname = "nixpkgs-pr-build-bot";
            version = "0-unstable";

            src = ./.;

            cargoLock.lockFile = ./Cargo.lock;

            nativeBuildInputs = with nixpkgsFor.${lib.elemAt (lib.splitString "-cross-" system) 0}; [ ];

            buildInputs = [ ];

            # enable unstable Rust features :)
            env.RUSTC_BOOTSTRAP = "1";

            meta = {
              description = "nixpkgs PR build bot";
              homepage = "https://github.com/FliegendeWurst/nixpkgs-pr-build-bot";
              license = lib.licenses.agpl3Plus;
              maintainers = with lib.maintainers; [ fliegendewurst ];
              mainProgram = "nixpkgs-pr-build-bot";
            };
          };
        });

      defaultPackage = forAllSystems (system: self.packages.${system}.nixpkgs-pr-build-bot);
    };
}
