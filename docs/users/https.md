# HTTPS certificate trust

HTTPS downloads use the operating system's certificate trust store. Private
certificate authorities belong in that store or in a PEM CA bundle selected with
`SSL_CERT_FILE`. `SSL_CERT_DIR` selects an OpenSSL-style certificate directory.
These settings apply to remote indexes, pack indexes, and packs.

```bash
SSL_CERT_FILE=/path/to/ca-bundle.pem elfshaker clone https://example.com/index.esi destination
```

The selected trust configuration must include the CAs needed for the server and
any HTTPS redirects. Certificate verification remains enabled.
