# Contrib scripts

This directory contains scripts which make use of elfshaker, for example
scripts to build a [manyclangs](https://github.com/elfshaker/manyclangs) pack.

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

### Relinking with a standalone Clang kit

`manyclangs-run` normally finds the Ubuntu compatibility `clang-12` and
`ld.lld-12` from `PATH`. To prove a standalone kit as the relink driver without
changing pack generation, set `MANYCLANGS_RELINK_KIT` to its extracted root:

```
MANYCLANGS_RELINK_KIT=/path/to/kit \
  manyclangs-run /path/to/extracted-snapshot COMMITISH clang
```

The kit must provide executable `bin/clang`, `bin/clang++`, and `bin/ld.lld`.
The run path passes those as `LINKSCRIPT_*` overrides to the snapshot's existing
`link.sh`; it does not copy the kit into a pack. This proof path runs directly
on the snapshot architecture. Docker-QEMU execution remains on the legacy
environment until the kit is explicitly made available inside the container.

To build new snapshots with the same kit, set `MANYCLANGS_RELINK_KIT` while
running `manyclangs-build-month`. The build scripts select its unsuffixed
drivers and LLD, and configure LLVM with `LLVM_ENABLE_TERMINFO=Off` because the
kit intentionally omits the Ubuntu terminfo library. This is a new snapshot
profile; it does not retrofit older Ubuntu-compatibility snapshots.

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
