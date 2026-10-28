# The Tectonic engine: how Galley gets its packages

Status: in use since 2026-10-27, with Tectonic 0.17.0 and bundle v33. The numbers below were
measured in an Ubuntu 24.04 VM on that date.

## 1. In short

Tectonic is Galley's default compiler. A LaTeX document loads packages, fonts and styles, and
Tectonic reads all of them from a *bundle*: one archive of about 135,000 files from TeX Live.

Galley installs the whole bundle once, unpacked into a read-only folder, next to the Tectonic
binary. Every compile reads from that folder and nothing else. So:

- **Builds never use the network.** No package is downloaded during a build, and a document
  cannot reach the internet.
- **A document cannot change what the next one reads.** The binary, the bundle and the format
  files are mounted read-only in the sandbox.
- **Every document gets the same packages,** whichever project, whichever machine.
- **The cost is disk:** 2.9 GB per machine, once, instead of a cache per project.

## 2. Why not download packages when a document needs them

Tectonic normally does exactly that: it fetches each file the first time a document asks for it
and keeps it in a cache. Galley used to work that way, and later in a safer variant (the author's
document ran offline, and Galley fetched what was missing with a small document of its own).

Both had problems:

- **Online with the author's document:** a document ran with internet access and with write access
  to a cache that later documents read. That lets one author plant files for another.
- **Fetching by reading errors:** Galley had to guess the missing file from LaTeX's error message.
  On 15 test documents, only 4 compiled from an empty cache, against 12 before. It missed fonts
  named by file, babel and polyglossia languages, BibTeX styles and font `.fd` files (which were
  silently replaced), and every package after the first missing one. Tectonic cannot report the
  files it looked for, so this could not be fixed.

With the whole bundle installed, the same 12 documents compile, offline and read-only. The 3 that
fail failed before too: a misspelled package, a font named only by family through fontspec, and an
`\input` of a file that does not exist. Four real papers (an 8-page ACM paper, a 6-page IEEE paper,
a 25-page thesis, a 24-frame beamer talk) compile in 1.4 to 6.4 s on one core.

A zip of the bundle (1.5 GB instead of 2.9 GB) also works, but every compile was 5 to 15% slower,
so the bundle is kept unpacked.

## 3. What is installed, and where

```
engine-<KEY>/                 read-only (folders 0555, files 0444)
  tectonic                    the pinned binary
  bundle/                     every bundle file, plus SHA256SUM
  cache/formats/<digest>-latex-33.fmt   built once at install time
```

`KEY` names the bundle and the binary: the first 16 hex digits of the bundle digest, a dash, and
the first 16 of the binary's SHA-256. Format files built for one binary and bundle are never used
with another.

Galley looks for the folder in two places, in this order:

1. `/opt/galley/tectonic/` — the container image built with `--build-arg WARM_TECTONIC=1`.
2. `<data_dir>/tectonic/` — where `galley engine install` puts it on a self-hosted machine.

A folder only counts when it is complete: the binary, a `bundle/SHA256SUM` equal to the pinned
digest, and a finished `.fmt` file. If none is found, Tectonic builds are refused with the
message "Tectonic is not installed. Run `galley engine install` once", and `galley doctor` says
the same. TeX Live engines are not affected.

## 4. What every compile gets

| In the sandbox | From | Access |
|---|---|---|
| `/galley/bin/tectonic` | `engine-<KEY>/tectonic` | read-only |
| `/galley/bundle` | `engine-<KEY>/bundle` | read-only |
| `/galley/cache` | `engine-<KEY>/cache` | read-only |
| `/tmp/galley-out` | this run's own scratch folder | writable |

Tectonic runs with `-b /galley/bundle --only-cached` and no network. Its other cache variables
point at the run's private `/tmp` (or at the run's folder when there is no sandbox), never at a
shared folder.

## 5. Installing

**Self-hosted:** run once, as the user Galley runs as:

```sh
galley engine install
```

It downloads 2.9 GB and needs about 6 GB free while it installs. It took 402 s in our VM. With
a copy of the pinned tar already on disk (an air-gapped machine, CI):

```sh
galley engine install --bundle-file /path/to/default_bundle_v33.tar
```

`deploy/install.sh` runs the install for you.

**Container image:** build with `--build-arg WARM_TECTONIC=1`. The image's engine stage depends
only on `deploy/tectonic.pins` and `deploy/tectonic-engine.sh`, so Docker reuses it until the
pins change, and a code change pushes no new bundle. Without the argument, the image has no
engine.

**On Fly:** both the server and the workers need the engine, so both apps' `fly.toml` need
`WARM_TECTONIC = '1'` under `[build.args]`. The server checks the engine before it queues a
build, and still compiles some jobs itself.

## 6. How the install is checked

Every value comes from `deploy/tectonic.pins`, which the Rust code and the shell script both
read, so they cannot drift:

- The binary's release archive and the binary inside it must match their pinned SHA-256. The
  archive hashes were checked against GitHub's published digests.
- The bundle must have exactly the pinned size, SHA-256 (over the whole file, including anything
  after the tar's end), number of files, and `SHA256SUM` content.
- Only regular files with a plain one-part name are accepted. Each is created new, without
  following a link, so a link, a folder, a device, a path or a duplicate name stops the install.
- Downloads use https only, with at most 5 redirects.
- The engine is filled under a temporary `.engine-*.partial` name and renamed into place in one
  step, so a stopped install never looks complete. Galley removes partial folders older than six
  hours at startup.

## 7. Upgrading Tectonic or the bundle

1. Change the values in `deploy/tectonic.pins` together: version, archive and binary hashes for
   both architectures, bundle URL, size, SHA-256, digest and file count.
2. Recompute both keys. The unit test `the_pins_file_parses_and_its_keys_match_the_hashes` fails
   if a key does not match the hashes.
3. Run `bash scripts/test-engines.sh`. It builds the engine with the new pins and runs the engine
   tests.
4. Deploy. On startup Galley removes engine folders with another key, so the old 2.9 GB goes.
   Figure caches are rebuilt, because their key includes the engine key.

## 8. What changed for existing installs

- `[build] tectonic_path` is gone. Only the pinned, hash-checked binary is used. An old
  `galley.toml` that still sets it loads fine; the line is ignored.
- At startup Galley removes the old shared cache (`<data_dir>/tectonic/cache`), its trust marker
  and the old downloaded binary. Each project's old package cache
  (`.galley/scratch/tectonic-cache`) is removed at that project's next build.
- Until `galley engine install` has run, Tectonic builds are unavailable.
- `fetched_packages` stays in the build result for API compatibility, and is always `false`.

## 9. Costs

- **Disk:** 2.9 GB per machine. It replaces 45 to 65 MB per project.
- **Image:** about 3 GB larger.
- **Fly:** a running machine's root filesystem is not billed separately. A stopped one costs
  $0.15 per GB per month, so about $0.44 per stopped machine, at most about $1.75 per month for
  the four demo machines. Downloads into Fly are free.

## 10. Known limits

- On a throttled shared-CPU machine, a large paper may hit the 120 s compile limit; in the VM at a
  quarter core, compiles were 15 to 18 times slower. Not yet checked on real Fly workers.
- The bundle comes from `relay.fullyjustified.net`, which has moved before. A move breaks new
  installs, never their content (the hash still protects it). Mirroring the tar to our own storage
  would remove that dependency.
- A document that needs a package outside TeX Live 2022 (the bundle's base) fails, as it would with
  any fixed TeX distribution. TeX Live engines are the way out for those.

## 11. Where the code is

- `deploy/tectonic.pins`: every pinned value.
- `deploy/tectonic-engine.sh`: the same install for the image build and CI.
- `crates/galley-build/src/install.rs`: `galley engine install`, its checks, and startup tidying.
- `crates/galley-build/src/engine.rs`: `Tectonic::locate` and the compile's mounts and arguments.
- `crates/galley-cli/src/doctor.rs`: what `galley doctor` reports.
