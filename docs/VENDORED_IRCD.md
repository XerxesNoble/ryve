# Vendored ngIRCd

Ryve ships its own ngIRCd binary so the workshop-local IRC backbone —
the coordination channel between Hands, Heads, and Atlas — works on
every install with zero configuration and no outbound network. Every
daemon invocation Ryve makes goes through
`crate::bundled_ircd::bundled_ircd_path()` (source: [`src/bundled_ircd.rs`](../src/bundled_ircd.rs)),
which resolves to the bundled binary at a fixed, layout-dependent
location (see *Runtime resolution* below).

## Provenance

The bundled binary is built from upstream ngIRCd sources hosted on the
project's release server:

- **Project:**       ngIRCd — <https://ngircd.barton.de/>
- **Release page:**  <https://ngircd.barton.de/download.php.en>
- **Source URL:**    `https://ngircd.barton.de/pub/ngircd/ngircd-<VERSION>.tar.gz`
- **Pinned version** (this repo): the exact tag is recorded in
  [`vendor/ircd/VERSION`](../vendor/ircd/VERSION). The current pinned
  version is **27** (released 2024-05-20).

No third-party mirrors, forks, or patched sources are used. The tarball
is downloaded at build time; its contents are compiled locally and only
the resulting `ngircd` executable is copied into `vendor/ircd/bin/ngircd`.
No source trees are checked in.

The binary itself is **not** checked into git (see
[`.gitignore`](../.gitignore)): compiled binaries are machine-specific,
so a macOS arm64 build will not run on Linux and vice versa. Instead it
is (re)produced per checkout — see *Building* below.

## License

ngIRCd is distributed under the GNU General Public License v2 or later
(see <https://github.com/ngircd/ngircd/blob/master/COPYING>). The GPL-2+
is compatible with Ryve's AGPL-3.0 license for the purpose of
distributing the compiled `ngircd` binary alongside the Ryve
application. When Ryve is shipped as a `.app` bundle or tarball, the
ngIRCd source URL and its license text MUST be included in the
accompanying third-party notices:

```
ngIRCd (<https://ngircd.barton.de/>) — GNU General Public License v2 or later.
Copyright (c) 2001-2024 Alexander Barton and contributors.
Source: https://ngircd.barton.de/pub/ngircd/ngircd-<VERSION>.tar.gz
```

ngIRCd's optional native dependencies (SSL/TLS, PAM, ident, iconv,
tcp-wrappers) are all disabled at configure time (see
`scripts/build-vendored-ircd.sh`), so no additional redistribution
obligations arise from transitive libraries: the shipped daemon links
only against the system C library.

## Building

Two entry points, both end at the same artefact:

### 1. Automatic — `cargo build` / `cargo run`

`build.rs` detects a missing `vendor/ircd/bin/ngircd` (or a stamp-file
mismatch — see *Stamp-file semantics* below) on unix hosts and invokes
`scripts/build-vendored-ircd.sh` to produce it. The first build after a
fresh clone therefore includes a one-time ngIRCd compile (~10–20 s on
modern hardware); subsequent builds skip the step because the binary is
already in place and the stamp matches the pinned version.

Set `RYVE_SKIP_VENDORED_IRCD_BUILD=1` to disable the auto-build (useful
when staging a pre-built binary into `vendor/ircd/bin/ngircd` from CI,
or when working offline with an already-built binary).

### 2. Manual — `./scripts/build-vendored-ircd.sh`

Run the script directly to (re)build the binary on demand, for example
after bumping `vendor/ircd/VERSION`:

```sh
./scripts/build-vendored-ircd.sh
```

The script downloads the pinned ngIRCd release, configures and compiles
it from source, and places the binary at `vendor/ircd/bin/ngircd`. Pass
`--prefix <dir>` to write the binary into an alternative location (used
by CI artifact staging).

### Prerequisites

**macOS:**

```sh
xcode-select --install        # provides cc + make
```

**Linux (Debian/Ubuntu):**

```sh
sudo apt-get install build-essential
```

The script configures with `--without-iconv --without-ident
--without-tcp-wrappers --without-pam --disable-ipv6`. Ryve runs the
daemon on localhost for workshop-scoped agent traffic, so
plaintext-only + no PAM + IPv4-only is the deliberate minimum
dependency surface. The only hard requirements are a working C compiler
and `make`.

## Stamp-file semantics

`scripts/build-vendored-ircd.sh` writes the version it just built into
`vendor/ircd/bin/.version` (the "stamp file"). `build.rs` reads the
stamp on every invocation via the shared helpers in
[`build_vendored_tmux_support.rs`](../build_vendored_tmux_support.rs)
and decides whether to re-run the script:

| Condition                                              | Auto-build runs? |
|--------------------------------------------------------|------------------|
| `vendor/ircd/bin/ngircd` missing                       | Yes              |
| Binary present, `.version` absent                      | Yes              |
| Binary present, `.version` ≠ `vendor/ircd/VERSION`     | Yes              |
| Binary present, `.version` == `vendor/ircd/VERSION`    | No               |

This is why a bare `vendor/ircd/VERSION` bump is enough to trigger a
rebuild on the next `cargo build` — the stamp will still read the old
version, the contents compare unequal, and `build.rs` kicks the
script. Deleting `vendor/ircd/bin/` resets both the binary and the
stamp so the next build is a clean first build.

The stamp pattern is shared with the vendored tmux build; the helpers
live in `build_vendored_tmux_support.rs` and are exercised by
`tests/vendored_tmux_stamp.rs`.

## Update process

Bumping ngIRCd is a four-step workflow. Do not skip step 4.

1. **Edit the pin.** Update `vendor/ircd/VERSION` to the new release
   tag (e.g. `27.1`). The value must match an ngIRCd release tarball at
   <https://ngircd.barton.de/pub/ngircd/>.
2. **Rebuild and smoke-test.** Remove the stale binary and rebuild:
   ```sh
   rm -f vendor/ircd/bin/ngircd vendor/ircd/bin/.version
   ./scripts/build-vendored-ircd.sh
   ./vendor/ircd/bin/ngircd --version    # should echo the new version
   ```
3. **Run the Rust test suite** to confirm the `PINNED_IRCD_VERSION`
   constant, `bundled_ircd_path()` resolver, and the end-to-end boot
   path all still behave:
   ```sh
   cargo test -p ryve bundled_ircd
   cargo test --test bundled_ircd_e2e   # fresh init → connected IRC client → event
   cargo test -p ipc --test lifecycle   # runtime boot / relay / inbound listener
   ```
4. **Update third-party notices.** If the new release changes ngIRCd's
   license or adds a copyright holder, update the notice in *License*
   above and the packaging notices that ship with the `.app` bundle /
   tarball.
5. **Commit** `vendor/ircd/VERSION` (and any notice changes) with a
   message like `chore: bump vendored ngIRCd to 27.1`.

## Opt-out

A single environment variable disables the auto-build at compile time:

```sh
RYVE_SKIP_VENDORED_IRCD_BUILD=1 cargo build
```

Semantics:

- **Respected by `build.rs`.** `build.rs` emits
  `cargo:rerun-if-env-changed=RYVE_SKIP_VENDORED_IRCD_BUILD`, so
  toggling the variable reliably re-runs the build script on the next
  compile.
- **Does not disable IRC at runtime.** `bundled_ircd_path()` still
  resolves whatever binary is on disk. When set on a fresh clone with
  no pre-staged binary, the resolver returns `None`, the workshop-side
  supervisor skips cleanly, and the runtime will try the configured
  IRC address (falling back to "no IRC this boot" with a flare ember
  if the dial fails).
- **Intended uses.** CI jobs that stage a pre-built binary before
  calling `cargo build`; offline development against an
  already-compiled binary; minimal images that only run `cargo check`
  or `cargo clippy` and don't need a working daemon.

## Runtime resolution

`src/bundled_ircd::bundled_ircd_path()` resolves the ngIRCd binary at
runtime:

1. **Installed layout:** `<exe_dir>/bin/ngircd` — used in the macOS
   `.app` bundle and Linux tarball.
2. **Development layout:** `<repo>/vendor/ircd/bin/ngircd` — used
   during `cargo run`. The compile-time path is baked in by `build.rs`
   as `RYVE_IRCD_DEV_PATH`.

The function returns `None` if neither path exists on disk. The
workshop-scoped supervisor (`src/ircd_process::IrcdSupervisor`)
consults `bundled_ircd_path()` when building its `SpawnSpec` and
skips the entire supervisor chain when the binary is missing, so a
checkout without a built daemon degrades gracefully rather than
crashing on boot.

## Packaging

When building a distributable artifact, the packaging step must copy
`vendor/ircd/bin/ngircd` (or a freshly-built binary) into the correct
location:

| Format          | ngIRCd path                          |
|-----------------|--------------------------------------|
| macOS `.app`    | `Ryve.app/Contents/MacOS/bin/ngircd` |
| Linux tarball   | `ryve/bin/ngircd`                    |

## Non-goals

- **Windows:** ngIRCd is not shipped on Windows.
- **User-swappable daemon:** Users cannot substitute a custom ngIRCd
  build through the Ryve UI. An explicit `irc_server` override in
  `WorkshopConfig` *does* point the Ryve IPC runtime at a different
  IRC server (useful for mesh setups spanning multiple machines), but
  the bundled daemon itself is not a swappable dependency.
- **Exposure beyond loopback:** the generated `.ryve/ircd/ircd.conf`
  binds `127.0.0.1` only. The daemon is never meant to serve external
  clients.
