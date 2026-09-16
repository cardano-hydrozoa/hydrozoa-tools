{
  description = "Operator tools for a Hydrozoa head: hztop, and a reader for a head's RocksDB store";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, utils }:
    utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };

        # RustRover keeps a writable copy of the stdlib sources under
        # ~/.cache/JetBrains/RustRover*/intellij-rust/stdlib-local-copy/<ver>-<hash>.
        # Copying from the read-only nix store preserves the missing write bit,
        # so its own copy attempt dies half-way and sync reports a "corrupted
        # standard library". This repairs any broken copy it left behind.
        fix-rustrover-stdlib = pkgs.writeShellApplication {
          name = "fix-rustrover-stdlib";
          text = ''
            src=${pkgs.rustPlatform.rustLibSrc}
            ver=${pkgs.rustc.version}
            shopt -s nullglob
            for dir in "$HOME"/.cache/JetBrains/RustRover*/intellij-rust/stdlib-local-copy/*/; do
              [ -w "$dir" ] && [ -f "$dir/Cargo.toml" ] && continue
              name=$(basename "$dir")
              chmod -R u+w "$dir"
              rm -rf "$dir"
              # Only refill copies of the toolchain this flake pins; broken
              # leftovers from other versions are just removed.
              if [[ "$name" =~ ^"$ver"-[0-9a-f]{40}$ ]]; then
                echo "fix-rustrover-stdlib: repairing $dir"
                mkdir -p "$dir"
                cp -rT --no-preserve=mode,ownership "$src" "$dir"
                chmod -R u+w "$dir"
              else
                echo "fix-rustrover-stdlib: removed stale broken $dir"
              fi
            done
          '';
        };
      in {
        packages.fix-rustrover-stdlib = fix-rustrover-stdlib;

        devShell = with pkgs;
          mkShell {
            buildInputs = [
              cargo
              rustc
              rustfmt
              rustPackages.clippy
              rust-analyzer
              rustPlatform.rustLibSrc

              # One path holding the whole toolchain, for an IDE that wants to be
              # pointed at a single directory rather than four. Give this path to
              # RustRover as the toolchain location.
              (symlinkJoin {
                name = "rust-toolchain";
                paths = [ cargo rustc rustfmt rustPackages.clippy ];
              })

              # Prebuilt RocksDB, linked instead of compiling the bundled C++
              # source in librocksdb-sys (see ROCKSDB_LIB_DIR below).
              rocksdb

              pkg-config
              just

              fix-rustrover-stdlib
            ];

            # Heal RustRover's stdlib cache on shell entry (no-op when intact).
            shellHook = ''
              fix-rustrover-stdlib
            '';

            # Give this to RustRover as the stdlib source.
            RUST_SRC_PATH = "${rustPlatform.rustLibSrc}";
            LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";

            # Link librocksdb-sys against the prebuilt RocksDB above rather than
            # recompiling its bundled C++ source -- minutes and ~950 MB per build
            # configuration, every time a build fingerprint changes. The crate's
            # build.rs skips build_rocksdb() when ROCKSDB_LIB_DIR is set;
            # INCLUDE_DIR points bindgen at the matching headers. nixpkgs rocksdb
            # (10.x) is C-API compatible with the crate's bundled 10.4.2 -- bump
            # these in lockstep if that ever diverges.
            ROCKSDB_LIB_DIR = "${rocksdb}/lib";
            ROCKSDB_INCLUDE_DIR = "${rocksdb}/include";
          };
      });
}
