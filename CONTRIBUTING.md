# Contributing to Flowdepth

Thanks for helping improve Flowdepth. The public project is named Flowdepth; the Rust package and application binary remain named `flowsurface` for upstream compatibility.

## Development setup

1. Install [Rust with rustup](https://www.rust-lang.org/tools/install).
2. Clone the repository and enter it:

   ```bash
   git clone https://github.com/Niketion/flowdepth.git
   cd flowdepth
   ```

3. Let rustup install the toolchain pinned in `rust-toolchain.toml`, then build the workspace:

   ```bash
   rustup show
   cargo build --workspace --all-targets --all-features --locked
   ```

On Debian or Ubuntu, install the native build dependencies with:

```bash
sudo apt install build-essential pkg-config libasound2-dev
```

See the platform requirements in the [README](./README.md#build-from-source) for macOS, Windows, Arch Linux, and Fedora.

## Quality checks

Run the same checks used by Develop CI before opening a pull request:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
```

Use `cargo fmt --all` to apply formatting locally.

## Indicator development

See [Indicator modules](docs/indicators.md) for catalog registration, runtime adapters, independent settings editors and the optional GPU interaction tests.

## Changes, commits, and pull requests

- Keep each commit focused and use a short, imperative subject such as `Fix reconnect status reporting`.
- Explain the problem, motivation, and user-visible effect in the pull request.
- Link relevant issues and include screenshots for UI changes.
- Describe any compatibility impact on saved layouts or cached market data.
- Keep unrelated refactors out of the same pull request.
- Open an issue before beginning a major architectural change so its scope and compatibility implications can be discussed.

Pull requests should target `develop` unless a maintainer requests otherwise. A pull request is expected to pass formatting, check, Clippy, and the existing test suite.

## Data and secrets

Do not commit proprietary market data, licensed datasets, private user data, exchange credentials, API keys, signing material, or other secrets. Use minimal synthetic or redistributable fixtures when a test needs data, and document its source and license when applicable.

By contributing, you agree that your contribution is distributed under the repository's [GPLv3-or-later license](./LICENSE) and that you will follow the [Code of Conduct](./CODE_OF_CONDUCT.md).
