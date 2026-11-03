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

Run checks relevant to your changes during development. Before publishing a pull request, run the complete quality gate used by [Develop CI](.github/workflows/develop-ci.yml) against the final changes. Run it again before publishing subsequent code changes to that PR.

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo run --locked -p flowsurface-indicator-engine --bin flowsurface-indicator -- test data/indicators/sources --namespace core
cargo run --locked -p flowsurface-indicator-engine --bin flowsurface-indicator -- test indicators/sources --namespace catalog
```

Use `cargo fmt --all` to apply formatting locally. CI sets `RUSTFLAGS="-D warnings"`; use the equivalent environment setting for your shell when reproducing CI. Keep the pinned toolchain and lockfile unless updating them is part of the change.

Add regression tests for bug fixes when they can meaningfully demonstrate the failure and fix. If a required check fails or cannot run, report the command and reason in the PR and clearly identify it as not fully validated.

### Manual application runs

For coding agents, launch the desktop application only when the user explicitly requests it. Building the project and running automated tests do not imply permission to launch the GUI.

When a manual run is requested, build the requested branch or commit and verify that the launched executable corresponds to those sources. Prefer an isolated runtime configuration using `FLOWSURFACE_DATA_PATH` to protect existing saved layouts and settings. Report what you actually verified and any limitations.

## Indicator development

See [Indicator modules](docs/indicators.md) for catalog registration, runtime adapters, independent settings editors and the optional GPU interaction tests.

## Changes, commits, and pull requests

- Use English for code, comments, documentation, commit messages, PR titles, and PR descriptions.
- Use Conventional Commits with a required module scope: `type(scope): message`.
- Choose a standard type such as `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, or `revert`. Use a short scope identifying the affected module.
- Keep each commit focused and use a concise, imperative message, for example `fix(derive): skip trades with missing batch status` or `docs(agents): define repository working agreements`.
- Use the same title format for PRs and squash or merge commits authored by contributors or agents.
- Explain the problem, motivation, and user-visible effect in the pull request.
- Link relevant issues and include screenshots for UI changes.
- Describe any compatibility impact on saved layouts or cached market data.
- Keep unrelated refactors out of the same pull request.
- Open an issue before beginning a major architectural change so its scope and compatibility implications can be discussed.

Use a dedicated branch for new work and target `develop` unless a maintainer requests otherwise. Follow the [PR template](.github/PULL_REQUEST_TEMPLATE.md), including validation and compatibility details. A pull request is expected to pass the complete quality gate above.

Coding agents should follow [AGENTS.md](AGENTS.md). They may commit, push, and open or update PRs within an explicitly authorized task; merging PRs and creating or publishing releases require an explicit user request.

## AI-assisted contributions

AI-assisted development is welcome. **If any submitted code was developed with AI assistance, you must explicitly disclose this in the PR description.** Briefly describe which parts were assisted; using AI does not disqualify a contribution.

For example: "AI assistance: Codex helped implement the Derive response parsing and regression tests."

Contributors remain responsible for understanding, reviewing, and validating all submitted code, including AI-assisted code.

## Data and secrets

Do not commit proprietary market data, licensed datasets, private user data, exchange credentials, API keys, signing material, or other secrets. Use minimal synthetic or redistributable fixtures when a test needs data, and document its source and license when applicable.

By contributing, you agree that your contribution is distributed under the repository's [GPLv3-or-later license](./LICENSE) and that you will follow the [Code of Conduct](./CODE_OF_CONDUCT.md).
