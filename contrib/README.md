# Contrib scripts

This directory contains scripts which make use of elfshaker, for example
scripts to build a [manyclangs](https://github.com/elfshaker/manyclangs) pack.

## Consuming manyclangs packs

Use a build of elfshaker containing `run` and mutable remote-pack refresh.
Create a repository from the publisher's `.esi` URL, then refresh it explicitly:

```sh
elfshaker clone https://your-publisher.example/index.esi manyclangs-store
cd manyclangs-store
elfshaker update
elfshaker find COMMIT_PREFIX
mkdir -p bin
elfshaker run --verify COMMIT_PREFIX -- bash ./link.sh --and-run clang --version
```

`update` verifies indexes and hashes cached payloads. Unchanged monthly packs
stay cached; changed payloads are discarded and downloaded when next needed.
Run `update` once after upgrading an older store. On a checksum failure during
publication, retry update and then run after the publisher finishes. Neither
`run` nor the wrapper below refreshes the catalog automatically.

`run` requires a unique substring match; `PACK:SNAPSHOT` disambiguates explicitly.
It extracts into the current repository directory and holds its lock until the
command finishes. That directory changes when another snapshot is selected.
Avoid nested elfshaker commands on the same locked store.

Linking stays in each snapshot's `link.sh`. Install a compatible linking
compiler, or select one with `LINKSCRIPT_CC`, `LINKSCRIPT_CXX` and
`LINKSCRIPT_LLD`. When needed, set `SSL_CERT_FILE` to a complete CA bundle for
the endpoint. These settings are independent of elfshaker's generic execution.
The historical packs and platform instructions are in the
[manyclangs project](https://github.com/elfshaker/manyclangs).

### Git refs and bisection

From an LLVM Git checkout, the thin adapter resolves a Git ref and delegates
extraction/execution to `elfshaker run`:

```sh
/path/to/elfshaker/contrib/manyclangs-run /absolute/path/to/manyclangs-store HEAD clang --version
git bisect run /path/to/elfshaker/contrib/manyclangs-run /absolute/path/to/manyclangs-store HEAD clang /absolute/path/to/reproducer.c
```

The tool is linked on every invocation. Lookup, extraction and linking failures
return 125; otherwise the tool's exit status is preserved. Use absolute paths
for input files outside the extraction directory. The generic equivalent for
link-and-run is:

```sh
sha=$(git rev-parse --short=10 HEAD)
(cd /absolute/path/to/manyclangs-store && mkdir -p bin &&
 elfshaker run --verify "$sha" -- bash ./link.sh --and-run clang --version)
```

Here a link failure is the command's status, not automatically a bisect skip;
the wrapper provides that manyclangs-specific mapping.

### Docker/QEMU

Build the [manyclangs-qemu-aarch64 image](https://github.com/elfshaker/manyclangs/tree/main/docker-qemu-aarch64)
and configure the documented emulation prerequisites. Set
`USE_DOCKER_QEMU=aarch64` on the wrapper to link and run in that image. Direct use
is also possible from the store:

```sh
elfshaker run --verify COMMIT_PREFIX -- docker run --rm \
  --mount "type=bind,source=$PWD,target=/elfshaker" manyclangs-qemu-aarch64 \
  bash -c 'mkdir -p bin; exec bash ./link.sh --and-run "$@"' manyclangs clang --version
```

The host performs extraction; the container performs linking and execution.

# Building manyclangs packs

These scripts are used to build the packfiles hosted at
https://github.com/elfshaker/manyclangs/releases.

They are provided as a best-effort, you are welcome to try and run them, please
let us know if you encounter issues but support may be limited. Patches to fix
things are welcomed.

It's intended that these scripts work on aarch64 and x86_64. Only Ubuntu 20.04
is tested for now.

## Top level approach

*Software Dependencies* Are documented in `manyclangs-setup.sh`, which is known
to work on an Ubuntu 20.04 machine on aarch64 and x86_64. It only tries to
install what is unavailable, so if you run it and it doesn't try to do anything,
then the build is ready to go.

*Machine dependency* It's expected you have a big-ish machine (e.g. 64+ core,
64GiB RAM) before running the scripts, otherwise you'll need to tune parameters
via the environment in `manyclangs-build-month` and use non-ramdisk storage --
this should be done by reading the shell script and understanding what it does.
Given a 64 core machine, a typical average build rate is 300/hour and a pack
takes ~6 hours to build.

Once `manyclangs-setup.sh` has run succesfully it should be possible to run:

```
cd /dev/shm
time PATH=$HOME/elfshaker/contrib:$PATH \
     GIT_DIR=${HOME}/llvm-project/.git \
     $HOME/elfshaker/contrib/manyclangs-build-month 2022 04
```

Which will produce `*.pack` and `*.pack.idx` files in
`/dev/shm/elfshaker_data/packs`, which are the precious output. Everything else
can be discarded.

### Efficient use of one big machine

The primary use case is to have one-big-machine and build a
one-month-of-commits-packfile in one-go. Secondarily, we want to be able to keep
the current month uptodate. Given this philosophy, we want to keep the
one-big-machine as occupied as possible. Unfortunately there are various bits of
the clang build process which are rather serial, resulting in a lot of unused
CPU time.

* To mitigate this, multiple builds run in parallel, but that conflicts with
  making efficient use of incremental builds.
* Therefore, builds need to be incremental, and it's necessary to have multiple
  of them run in parallel.
  * Incremental builds start at a point in time and move forward
    commit-by-commit.
  * This results in a few tens-of-seconds build for a typical 'touch a few
    translation units' commit. However, such a build will only consume a few
    CPUs because they are bottlenecked on a small number (much fewer than
    `$NCPU`) of large translation units for that time.
  * Saturating the machine to use all of the available resource is achieved by
    building from multiple points in time simultaneously.
  * This introduces a problem of build contention, running more compiler
    procesess in parallel than necessary is wasteful and risks deadlocking the
    machine for example via an out-of-memory condition. To mitigate this, a
    jobserver-aware ninja is used, which ensure that only as many compilers run
    concurrently as there are available CPU cores.
* Ccache mitigates some incremental build misses.
* Builds happen entirely in tmpfs because the machine has enough RAM.
  * Total storage for 1 month is ~40G ccache (also in tmpfs) + a few gigabytes
    for the elfshaker dir + a few gigabytes for each incremental build.

