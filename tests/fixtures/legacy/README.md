# Legacy pack-index fixtures

These fixtures pin the two native `OsString` encodings used by pack-index
versions 1 and 2. Each index has the matching `.pack` beside it.

| Fixture | Producer | Command or derivation |
| --- | --- | --- |
| `linux-v1` | Linux, revision `3eff63909797` | Existing `verification.pack{,.idx}` artifacts from the last version 1 revision |
| `linux-v2` | Linux | Existing `test-scripts/artifacts/verification.pack{,.idx}` artifacts |
| `windows-v2` | Windows, revision `7cc7592aaad9` | Existing `verification-windows.pack{,.idx}` artifacts |
| `windows-v1` | Windows v2 fixture above | The file handles were decoded, their version 2 mode field was removed, and the index header was set to version 1; the path and object pools remain byte-for-byte Windows-produced data |

The Windows version 1 fixture is derived because the repository did not retain
a Windows artifact from before version 2. It retains the released Windows
binary's Serde `OsString::Windows` path representation while exercising the
version 1 file-handle shape.

SHA-256 checksums:

```text
e94d59777ef3eb5d73bb0d0a6ab94342dd0f3eed87051dbffd0d282bd7f69dfd  linux-v1.pack
6d0e984f5ea049bba8de020626e94819aa1f32f75c13004737f2d36b660a6e19  linux-v1.pack.idx
7e447870d0b4ca9a32c71150ee0b6c0ff1ca1c69b5fa6806427797acfd1cd7dd  linux-v2.pack
02521f6d98ed91cc846e30acbb1a8c67b7d63594c2dfa5d21d6a2b6ac787e21f  linux-v2.pack.idx
3c67fd7a48082216c8d8ae38db61923d9f2b7ad2ea82e550fa29ed5311f341cc  windows-v1.pack
21f4b9eb50a12e847964590c4b85b07701654651d860d330369030ccf98802b3  windows-v1.pack.idx
3c67fd7a48082216c8d8ae38db61923d9f2b7ad2ea82e550fa29ed5311f341cc  windows-v2.pack
d440c083caec42a1b48228db00297c03a09cdc1b47f4d2cec20583a716c8c128  windows-v2.pack.idx
```
