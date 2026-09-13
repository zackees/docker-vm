{
  description = "Private Docker desktop, built from source through Soldr";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/c5c4a43b0e8056328ec4529f735cabdb8f1942bb";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
      # Soldr is the compiler front door.  Pinning the release archive makes
      # the build engine itself part of this flake's reproducible closure.
      soldr = pkgs.stdenvNoCC.mkDerivation {
        pname = "soldr";
        version = "0.9.15";
        src = pkgs.fetchurl {
          url = "https://github.com/zackees/soldr/releases/download/v0.9.15/soldr-v0.9.15-x86_64-unknown-linux-gnu.tar.zst";
          hash = "sha256-3E+z9q7W/Yy38Iboh6h0nRdIorb1JJQ1rhtBACXkfAM=";
        };
        nativeBuildInputs = [ pkgs.zstd pkgs.autoPatchelfHook ];
        buildInputs = [ pkgs.stdenv.cc.cc.lib ];
        unpackPhase = "tar --use-compress-program=unzstd -xf $src";
        installPhase = ''
          install -Dm755 soldr $out/bin/soldr
          install -Dm755 soldr-daemon $out/bin/soldr-daemon
        '';
      };

      cargoVendor = pkgs.rustPlatform.importCargoLock {
        lockFile = ./viewer/src-tauri/Cargo.lock;
        outputHashes = {
          "tauri-2.11.5" = "sha256-7M6U/XuXIYrKHbs266OB80YxRfsqzUdPTay3k9B2+jA=";
        };
      };

      dockerVm = pkgs.stdenv.mkDerivation {
        pname = "docker-vm";
        version = "0.1.0";
        src = ./.;
        nativeBuildInputs = [
          pkgs.pkg-config pkgs.wrapGAppsHook3 pkgs.makeWrapper
          pkgs.copyDesktopItems soldr
        ];
        buildInputs = [
          pkgs.gtk3 pkgs.webkitgtk_4_1 pkgs.glib pkgs.libsoup_3
          pkgs.gst_all_1.gstreamer pkgs.gst_all_1.gst-plugins-base
          pkgs.gst_all_1.gst-plugins-good
        ];
        desktopItems = [ (pkgs.makeDesktopItem {
          name = "docker-vm";
          desktopName = "Private Desktop";
          comment = "Isolated, RAM-only Chromium desktop";
          exec = "docker-vm";
          icon = "docker-vm";
          terminal = false;
          categories = [ "Network" "RemoteAccess" ];
          startupWMClass = "docker-vm-viewer";
        }) ];
        buildPhase = ''
          runHook preBuild
          export DOCKER_VM_SOURCE_ROOT="$PWD"
          mkdir -p viewer/src-tauri/.cargo
          cp ${cargoVendor}/.cargo/config.toml viewer/src-tauri/.cargo/config.toml
          chmod u+w viewer/src-tauri/.cargo/config.toml
          sed -i 's|directory = "cargo-vendor-dir"|directory = "${cargoVendor}"|' \
            viewer/src-tauri/.cargo/config.toml
          cat >> viewer/src-tauri/.cargo/config.toml <<EOF
          [net]
          offline = true
          EOF
          export HOME="$TMPDIR/home"
          export CARGO_HOME="$TMPDIR/rustup-home"
          export SOLDR_CACHE_DIR="$TMPDIR/soldr-cache"
          # Nix supplies the host compiler and native libraries.  Disable
          # Soldr's optional network catalogue so this derivation stays pure.
          export SOLDR_MANIFEST_DISABLE=1
          mkdir -p "$HOME" "$SOLDR_CACHE_DIR" "$CARGO_HOME/bin"
          ln -s ${pkgs.cargo}/bin/cargo "$CARGO_HOME/bin/cargo"
          ln -s ${pkgs.rustc}/bin/rustc "$CARGO_HOME/bin/rustc"
          cd viewer/src-tauri
          # Use Soldr's Cargo front door: its compiler cache is active while
          # Nix supplies the native GTK/WebKit libraries for this host build.
          soldr cargo build --jobs 4 --release --offline
          runHook postBuild
        '';
        doCheck = true;
        checkPhase = ''
          cd "$DOCKER_VM_SOURCE_ROOT/viewer/src-tauri"
          soldr cargo test --jobs 4 --release --offline
        '';
        dontPatchShebangs = true;
        installPhase = ''
          runHook preInstall
          cd "$DOCKER_VM_SOURCE_ROOT"
          mkdir -p $out/libexec $out/bin $out/share/pixmaps $out/share/docker-vm
          cp viewer/src-tauri/target/release/docker-vm-viewer $out/libexec/
          cp -r images $out/share/docker-vm/images
          cp compose.yaml $out/share/docker-vm/
          cp viewer/src-tauri/icons/icon.png $out/share/pixmaps/docker-vm.png
          makeWrapper $out/libexec/docker-vm-viewer $out/bin/docker-vm \
            --run 'ulimit -c 0' \
            --set DOCKER_VM_RESOURCE_DIR $out/share/docker-vm \
            --set GDK_BACKEND x11 \
            --set LIBGL_ALWAYS_SOFTWARE 1 \
            --set WEBKIT_DISABLE_COMPOSITING_MODE 1 \
            --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.docker pkgs.docker-compose pkgs.zenity ]}
          runHook postInstall
        '';
        meta = {
          description = "Private Docker desktop canvas";
          platforms = [ system ];
          mainProgram = "docker-vm";
        };
      };
    in {
      packages.${system} = {
        default = dockerVm;
        docker-vm = dockerVm;
        inherit soldr;
      };
      devShells.${system}.default = pkgs.mkShell {
        packages = [ pkgs.cargo pkgs.rustc soldr pkgs.pkg-config ];
        buildInputs = dockerVm.buildInputs;
      };
    };
}
