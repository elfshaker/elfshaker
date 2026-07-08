# Portable symbolic-link fixture

`portable-symlinks-v3.pack{,.idx}` was produced on Linux with elfshaker at
revision `212663db`:

```text
elfshaker store snapshot
elfshaker pack portable-symlinks-v3 --compression-level 1 --threads 1 --frames 1
```

The snapshot contains a regular `target-file`, a populated `target-dir`, a
portable link from `file-link` to `target-file`, and a portable link from
`directory-link` to `target-dir`. The two link entries retain known file and
directory target-kind hints for Windows extraction.

SHA-256 checksums:

```text
da2c962a890a185055498adf88d3523435e2517a1c61be5b1224f775aeffbd40  portable-symlinks-v3.pack
d6898c29453b5e10d7edfa89ec64dca95e99c805ade77e655a35ca3b77cfeda6  portable-symlinks-v3.pack.idx
```
