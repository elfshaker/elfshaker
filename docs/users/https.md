# HTTPS certificate trust

HTTPS downloads use the operating system's certificate trust store. Private
certificate authorities belong in that store or in a PEM CA bundle selected with
`SSL_CERT_FILE`. `SSL_CERT_DIR` selects an OpenSSL-style certificate directory.
These settings apply to remote indexes, pack indexes, and packs.

Configured PEM locations replace the platform's default trust roots on every
supported platform, including macOS. Unreadable paths and selections containing
no CA certificates are reported and leave HTTPS without trusted roots; they
never fall back to the platform store. Plain HTTP requests do not require CA
certificates. With no configured locations, elfshaker uses native platform
verification, including the macOS Keychain.

```bash
SSL_CERT_FILE=/path/to/ca-bundle.pem elfshaker clone https://example.com/index.esi destination
```

The selected trust configuration must include the CAs needed for the server and
any HTTPS redirects. Certificate verification remains enabled.
