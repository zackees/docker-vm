{
  description = "Private Docker desktop, built from source through Soldr";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/c5c4a43b0e8056328ec4529f735cabdb8f1942bb";
  # The viewer embeds the noVNC submodule; without it the window loads no RFB
  # client and never leaves "Starting…".
  inputs.self.submodules = true;

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
      # Soldr is the compiler front door.  The release archive is pinned in
      # soldr.json (refreshed by scripts/update-soldr), so the build engine is
      # part of this flake's reproducible closure.  Consumers that track the
      # newest soldr pass their own pin through `mkSoldr` and `.override`.
      mkSoldr = pin: pkgs.stdenvNoCC.mkDerivation {
        pname = "soldr";
        inherit (pin) version;
        src = pkgs.fetchurl { inherit (pin) url hash; };
        nativeBuildInputs = [ pkgs.zstd pkgs.autoPatchelfHook ];
        buildInputs = [ pkgs.stdenv.cc.cc.lib ];
        unpackPhase = "tar --use-compress-program=unzstd -xf $src";
        installPhase = ''
          install -Dm755 soldr $out/bin/soldr
          install -Dm755 soldr-daemon $out/bin/soldr-daemon
        '';
      };
      soldr = mkSoldr (builtins.fromJSON (builtins.readFile ./soldr.json));

      # Soldr resolves the repo-pinned toolchain through rustup, which cannot
      # download inside the sandbox.  Present Nix's rustc as that rustup
      # toolchain instead: same version, nothing fetched.
      toolchainChannel = (builtins.fromTOML (builtins.readFile ./rust-toolchain.toml)).toolchain.channel;
      rustupToolchain = assert pkgs.lib.assertMsg (toolchainChannel == pkgs.rustc.version)
        "rust-toolchain.toml pins ${toolchainChannel}, but nixpkgs provides rustc ${pkgs.rustc.version}";
        "${toolchainChannel}-x86_64-unknown-linux-gnu";
      rustupShim = pkgs.writeShellScript "rustup" ''
        case "$1 $2" in
          "toolchain list") echo "${rustupToolchain} (default)" ;;
          "toolchain install") ;;
          *) echo "rustup (Nix shim): unsupported: $*" >&2; exit 1 ;;
        esac
      '';

      cargoVendor = pkgs.rustPlatform.importCargoLock {
        lockFile = ./viewer/src-tauri/Cargo.lock;
        outputHashes = {
          "tauri-2.11.5" = "sha256-7M6U/XuXIYrKHbs266OB80YxRfsqzUdPTay3k9B2+jA=";
        };
      };

      dockerVm = pkgs.lib.makeOverridable ({ soldr }: pkgs.stdenv.mkDerivation {
        pname = "docker-vm";
        version = "0.1.0";
        src = ./.;
        nativeBuildInputs = [
          pkgs.pkg-config pkgs.wrapGAppsHook3 pkgs.makeWrapper
          pkgs.copyDesktopItems soldr pkgs.clang pkgs.lld
          # Soldr's linker driver bypasses Nix's ld-wrapper, which is what
          # normally records library RUNPATHs; restore them from buildInputs.
          pkgs.autoPatchelfHook
        ];
        buildInputs = [
          pkgs.gtk3 pkgs.webkitgtk_4_1 pkgs.glib pkgs.libsoup_3
          pkgs.gst_all_1.gstreamer pkgs.gst_all_1.gst-plugins-base
          # gtkglsink: the viewer presents the NVENC stream natively on the
          # GPU in a GTK layer (WebKitGTK's WebCodecs->WebGL path copies every
          # frame through the CPU).
          (pkgs.gst_all_1.gst-plugins-good.override { gtkSupport = true; })
          # WebCodecs H.264 for the NVENC video transport: nvcodec (NVDEC) in
          # -bad, avdec as the software fallback.
          pkgs.gst_all_1.gst-plugins-bad pkgs.gst_all_1.gst-libav
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
          test -f viewer/ui/novnc/core/rfb.js || {
            echo "viewer/ui/novnc is empty: fetch this flake with submodules" >&2
            exit 1
          }
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
          export CARGO_HOME="$TMPDIR/cargo-home"
          export RUSTUP_HOME="$TMPDIR/rustup-home"
          export SOLDR_CACHE_DIR="$TMPDIR/soldr-cache"
          # Nix supplies the toolchain, linker and native libraries.  Disable
          # Soldr's network catalogue and rustup bootstrap so this stays pure.
          export SOLDR_MANIFEST_DISABLE=1 SOLDR_NO_BOOTSTRAP=1
          toolchain="$RUSTUP_HOME/toolchains/${rustupToolchain}"
          mkdir -p "$HOME" "$SOLDR_CACHE_DIR" "$CARGO_HOME/bin" "$toolchain/bin" "$TMPDIR/shim"
          ln -s ${pkgs.rustc.unwrapped}/lib "$toolchain/lib"
          ln -s ${pkgs.rustc.unwrapped}/bin/* ${pkgs.cargo}/bin/* "$toolchain/bin/"
          ln -s ${rustupShim} "$TMPDIR/shim/rustup"
          export PATH="$TMPDIR/shim:$PATH"
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
          # The viewer must be a GPU client: it presents the NVENC stream with
          # nvh264dec -> GL -> gtkglsink. Forcing LIBGL_ALWAYS_SOFTWARE /
          # WEBKIT_DISABLE_COMPOSITING_MODE made every viewer GL context Mesa
          # llvmpipe: 22.6 fps shown at 2560x1600 vs 58.4 (= the page's fps)
          # with hardware GL, and nothing crashed without them
          # (docs/GPU-PIPELINE.md).
          makeWrapper $out/libexec/docker-vm-viewer $out/bin/docker-vm \
            --run 'ulimit -c 0' \
            --set DOCKER_VM_RESOURCE_DIR $out/share/docker-vm \
            --set GDK_BACKEND x11 \
            --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.docker pkgs.docker-compose pkgs.zenity ]}
          runHook postInstall
        '';
        meta = {
          description = "Private Docker desktop canvas";
          platforms = [ system ];
          mainProgram = "docker-vm";
        };
      }) { inherit soldr; };
    in {
      packages.${system} = {
        default = dockerVm;
        docker-vm = dockerVm;
        inherit soldr;
      };
      # `mkSoldr pin` builds any soldr release from a soldr.json-shaped pin;
      # `docker-vm.override { soldr = ...; }` builds the viewer with it.
      lib.mkSoldr = mkSoldr;
      devShells.${system}.default = pkgs.mkShell {
        packages = [ pkgs.cargo pkgs.rustc soldr pkgs.pkg-config ];
        buildInputs = dockerVm.buildInputs;
      };
    };
}
