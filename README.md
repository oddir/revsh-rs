# revsh-rs

PoC reimplementation of [revsh](https://github.com/emptymonkey/revsh) control part in async Rust. Many features missing. Only basic shell (no tty) and socks4 proxy working. Still learning Rust :3

Convert revsh keys:

```
openssl pkcs12 -export -out identity.pfx -inkey key.pem -in cert.pem
```

Build project linking with same OpenSSL as the revsh target:

```
OPENSSL_LIB_DIR=/path/to/openssl OPENSSL_INCLUDE_DIR=/path/to/openssl/include cargo build --release
```

Run control:

```
target/release/control --keyfile ../revsh/keys/identity.pfx --listen-addr 0.0.0.0:2200 --proxy-addr 127.0.0.1:1080
```
