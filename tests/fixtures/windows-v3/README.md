# Windows-writer version 3 fixture

`windows-writer-v3.pack` is produced by the Windows GNU build of elfshaker. It
contains a portable snapshot with file, directory, and dangling symlinks, plus
a native snapshot whose absolute target is encoded as Windows UTF-16LE.

The checked-in fixture is generated locally by running the
`x86_64-pc-windows-gnu` binary under Wine 11.0. Wine and the Windows binary run
through QEMU x86-64 on an AArch64 Linux host. The source links are created by a
Windows Rust helper using `std::os::windows::fs::{symlink_file, symlink_dir}` in
the same environment. This proves interoperability with the Windows writer and
reader code paths; it is not evidence from Windows or NTFS.

`generate.ps1` reproduces the same inputs on Windows when native-host evidence
is wanted. It deliberately writes one compression frame with one worker so the
fixture remains deterministic.

SHA-256:

```text
71b73ec2a5f548845fdeb3fedd90f980abb590887b9fe6240384931ece2abf25  windows-writer-v3.pack
d8a9fa65d2ea5a119cd0473c8604b321e46f2f5193392aebd970acbed07bb8aa  windows-writer-v3.pack.idx
```
