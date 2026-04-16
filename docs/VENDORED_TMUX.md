# Vendored tmux

Ryve ships its own tmux binary so Hand and Head sessions behave identically on
every install, regardless of whether the user has tmux installed, which
version, or how it is configured. Every `tmux` invocation Ryve makes goes
through `src/bundled_tmux::bundled_tmux_path()`, which resolves to the bundled
binary at a fixed, layout-dependent location (see *Runtime resolution* below).

## Provenance

The bundled binary is built from upstream tmux sources hosted on GitHub:

- **Project:**       tmux — <https://github.com/tmux/tmux>
- **Release page:**  <https://github.com/tmux/tmux/releases>
- **Source URL:**    `https://github.com/tmux/tmux/releases/download/<VERSION>/tmux-<VERSION>.tar.gz`
- **Pinned version** (this repo): the exact tag is recorded in
  [`vendor/tmux/VERSION`](../vendor/tmux/VERSION). The current pinned version is
  **3.5a** (released 2024-10-07).

No third-party mirrors, forks, or patched sources are used. The tarball is
downloaded at build time; its contents are compiled locally and only the
resulting `tmux` executable is copied into `vendor/tmux/bin/tmux`. No source
trees are checked in.

The binary itself is **not** checked into git (see
[`.gitignore`](../.gitignore)): its native dependencies (libevent, ncurses)
are machine-specific, so a macOS arm64 build will not run on Linux or on a
macOS machine without Homebrew's libevent. Instead it is (re)produced per
checkout — see *Building* below.

## License

tmux is distributed under the permissive ISC license (see
<https://github.com/tmux/tmux/blob/master/COPYING>). The ISC license is
compatible with Ryve's AGPL-3.0 license for the purpose of distributing the
compiled `tmux` binary alongside the Ryve application. When Ryve is shipped
as a `.app` bundle or tarball, the `tmux` source URL and its license text MUST
be included in the accompanying third-party notices:

```
tmux (<https://github.com/tmux/tmux>) — ISC License.
Copyright (c) 2007 Nicholas Marriott and contributors.
Source: https://github.com/tmux/tmux/releases/download/<VERSION>/tmux-<VERSION>.tar.gz
```

The transitive native libraries (libevent, ncurses) are linked dynamically on
macOS and either dynamically or statically on Linux depending on the
`--enable-static` path (see the build script). Their licenses apply only when
the Ryve distribution bundles those libraries; for developer builds the
system-provided copies are used and no additional redistribution obligations
arise.

## Building

Two entry points, both end at the same artefact:

### 1. Automatic — `cargo build` / `cargo run`

`build.rs` detects a missing `vendor/tmux/bin/tmux` on unix hosts and
invokes `scripts/build-vendored-tmux.sh` to produce it. The first build after
a fresh clone therefore includes a one-time tmux compile (~15–30 s on modern
hardware); subsequent builds skip the step because the binary is already in
place.

Set `RYVE_SKIP_VENDORED_TMUX_BUILD=1` to disable the auto-build (useful when
staging a pre-built binary into `vendor/tmux/bin/tmux` from CI, or when
working offline with an already-built binary).

### 2. Manual — `./scripts/build-vendored-tmux.sh`

Run the script directly to (re)build the binary on demand, for example after
bumping `vendor/tmux/VERSION`:

```sh
./scripts/build-vendored-tmux.sh
```

The script downloads the pinned tmux release from GitHub, configures and
compiles it from source, and places the binary at `vendor/tmux/bin/tmux`.
Pass `--prefix <dir>` to write the binary into an alternative location (used
by CI artifact staging).

### Prerequisites

**macOS:**

```sh
brew install autoconf automake libevent pkg-config
```

**Linux (Debian/Ubuntu):**

```sh
sudo apt-get install build-essential autoconf automake libevent-dev libncurses-dev bison pkg-config
```

The script configures with `--disable-utf8proc` (tmux still handles UTF-8;
this just avoids a build-time dependency on libutf8proc). On Linux it
additionally passes `--enable-static` to reduce runtime library dependencies;
on macOS the system linker rejects fully static binaries, so we accept the
dynamic libevent/ncurses linkage.

## Update process

Bumping tmux is a four-step workflow. Do not skip step 4.

1. **Edit the pin.** Update `vendor/tmux/VERSION` to the new tag (e.g. `3.6`).
   The value must match a GitHub release tag at
   <https://github.com/tmux/tmux/releases>.
2. **Rebuild and smoke-test.** Remove the stale binary and rebuild:
   ```sh
   rm -f vendor/tmux/bin/tmux
   ./scripts/build-vendored-tmux.sh
   ./vendor/tmux/bin/tmux -V    # should echo the new version
   ```
3. **Run the Rust test suite** to confirm the `PINNED_TMUX_VERSION` constant
   and `bundled_tmux_path()` resolver still behave:
   ```sh
   cargo test -p ryve bundled_tmux
   cargo test -p ryve hand_spawn      # exercises tmux end-to-end
   ```
4. **Update third-party notices.** If the new release changes tmux's license
   or adds a copyright holder, update the notice in *License* above and the
   packaging notices that ship with the `.app` bundle / tarball.
5. **Commit** `vendor/tmux/VERSION` (and any notice changes) with a message
   like `chore: bump vendored tmux to 3.6`.

## Runtime resolution

`src/bundled_tmux::bundled_tmux_path()` resolves the tmux binary at runtime:

1. **Installed layout:** `<exe_dir>/bin/tmux` — used in the macOS `.app`
   bundle and Linux tarball.
2. **Development layout:** `<repo>/vendor/tmux/bin/tmux` — used during
   `cargo run`. The compile-time path is baked in by `build.rs` as
   `RYVE_TMUX_DEV_PATH`.

The function returns `None` if neither path exists on disk. `resolve_tmux_bin`
(in `src/tmux.rs`) consults `bundled_tmux_path()` after the `RYVE_TMUX_PATH`
and `RYVE_TMUX_BIN` env var overrides and before falling back to a system
`tmux` on `$PATH`.

## Packaging

When building a distributable artifact, the packaging step must copy
`vendor/tmux/bin/tmux` (or a freshly-built binary) into the correct location:

| Format          | tmux path                          |
|-----------------|------------------------------------|
| macOS `.app`    | `Ryve.app/Contents/MacOS/bin/tmux` |
| Linux tarball   | `ryve/bin/tmux`                    |

## Non-goals

- **Windows:** tmux is not shipped on Windows.
- **User-swappable:** Users cannot substitute a custom tmux build through the
  Ryve UI. `RYVE_TMUX_PATH` / `RYVE_TMUX_BIN` remain as developer-only escape
  hatches.
