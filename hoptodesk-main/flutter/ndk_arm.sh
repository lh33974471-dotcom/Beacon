#!/usr/bin/env bash
export OPENSSL_STATIC=1  # Build OpenSSL statically to ensure 16 KB alignment
export LDFLAGS="-Wl,-z,max-page-size=16384"  # For OpenSSL build
RUSTFLAGS='-C link-arg=-Wl,-z,max-page-size=16384' cargo ndk --platform 21 --target armv7-linux-androideabi build --release --features "flutter${EXTRA_CARGO_FEATURES:+ $EXTRA_CARGO_FEATURES}"
