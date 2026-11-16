# Grooviq

Grooviq is a free, open-source desktop DAW for beatmakers. It works the way FL Studio does, adds the best ideas from Ableton, costs nothing, and ships many built-in instruments and effects so you can finish a beat without buying plugins.

**Status: pre-alpha, not usable yet.**

## Build and test

```sh
cargo build --workspace
cargo test --workspace --all-features
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

`--all-features` turns on the engine's `test-hooks` feature; without it, the audio-health tests that inject a NaN or a panic do not run. The pinned toolchain is in `rust-toolchain.toml`.

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).
