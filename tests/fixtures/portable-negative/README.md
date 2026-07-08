# Producer-derived portability failures

These version 3 fixtures were produced on Linux with elfshaker at revision
`153a0079`. Each snapshot also contains a regular file named `sentinel` so
consumer tests can prove that preflight rejection happens before extraction
mutates an otherwise writable entry.

| Fixture | Producer-side path set | Expected compatibility |
| --- | --- | --- |
| `unix-non-utf8-v3` | `non-utf8-\xFF` | Extracts on Linux; rejected as unrepresentable on macOS and Windows |
| `windows-case-collision-v3` | `Case`, `case` | Extracts on Linux; rejected as a target collision on macOS and Windows |
| `macos-unicode-collision-v3` | `Résumé`, `Re\u0301sume\u0301` | Extracts on Linux and Windows; rejected as a normalization collision on macOS |

Each fixture was created with:

```text
elfshaker store snapshot
elfshaker pack NAME --compression-level 1 --threads 1 --frames 1
```

SHA-256 checksums:

```text
bb1e7cb92e288d82cbf2a7b98cc22acaba88522dd217b07541005b622f6933f9  macos-unicode-collision-v3.pack
178491ca29a4c29c799dca954b750908c33d61dcd06475e500544e471bfdb749  macos-unicode-collision-v3.pack.idx
37fc11fc5ec74693a885457e82c2189612d911879005d0a187db61c49129b120  unix-non-utf8-v3.pack
03157e50f50c3145e5056a5695bed0f5255fe48764a341726d94973e7a22697f  unix-non-utf8-v3.pack.idx
ee639a05541b17f85bc9f64ec0dd5713cc78acb5019620d4096bd45cb80fd425  windows-case-collision-v3.pack
0be8bf3315d82c58bcb743d896e1985aa529ed064b0d2c58d22fb4a14a8f965a  windows-case-collision-v3.pack.idx
```
