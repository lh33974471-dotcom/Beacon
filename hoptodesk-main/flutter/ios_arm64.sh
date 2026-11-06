#!/usr/bin/env bash
IPHONEOS_DEPLOYMENT_TARGET=15.0 cargo build --features flutter --release --target aarch64-apple-ios --lib
