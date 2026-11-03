# Flowdepth agent instructions

These instructions apply to this repository. Use [CONTRIBUTING.md](CONTRIBUTING.md)
for contributor guidance and [README.md](README.md) for setup and project context.

## Language and naming

- Use English for code, comments, documentation, commit messages, PR titles, and
  PR descriptions.
- Communicate in English unless the user requests another language.
- The project is named Flowdepth. Rust packages, the executable, and existing
  internal identifiers retain the `flowsurface` name for compatibility.

## Working agreements

- Inspect the relevant code and existing conventions before making changes.
- Keep changes focused on the requested task. Avoid unrelated refactoring.
- Make routine implementation choices and run relevant checks autonomously.
  Ask when a missing requirement materially affects the intended behavior.
- Preserve unrelated user changes. Use a dedicated branch for new work; PRs
  normally target `develop`.
- Commit, push, and open or update PRs when requested or needed to complete an
  explicitly authorized task. Do not ask again for authorization already given.
- Merge PRs and create or publish releases only when explicitly requested.
- Follow existing guidance on saved layouts, cache formats, data handling, and
  secrets in the contributor documentation. Report compatibility impacts in PRs.

## Commit and PR conventions

- Use Conventional Commits with a required module scope:
  `type(scope): message`.
- Use a standard type such as `feat`, `fix`, `docs`, `refactor`, `perf`, `test`,
  `build`, `ci`, `chore`, or `revert`.
- Choose a short scope describing the affected module, such as `derive`, `gex`,
  `charts`, `indicators`, or `agents`.
- Write a concise English imperative message and keep each commit focused.
- Examples:
  - `fix(derive): skip trades with missing batch status`
  - `feat(indicators): add a volume profile setting`
  - `docs(agents): define repository working agreements`
  - `chore(deps): update workspace dependencies`
- Use the same title format for PRs and squash or merge commit titles authored
  by the agent. Follow `.github/PULL_REQUEST_TEMPLATE.md` for PR descriptions.
- Explain the problem, resulting behavior, validation, and any compatibility
  impact. Do not claim a check passed unless it was actually run successfully.

## Validation

- During implementation, run checks relevant to the change. Add regression tests
  for bug fixes when they can meaningfully demonstrate the failure and fix.
- Before publishing a PR, run the complete quality gate below against the final
  changes. Run it again before publishing subsequent code changes to that PR.
- Use the Rust toolchain pinned in `rust-toolchain.toml` and preserve the lockfile
  unless dependency changes are part of the task.
- Treat `.github/workflows/develop-ci.yml` as the source of truth if its quality
  gate changes. Its current checks are:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo run --locked -p flowsurface-indicator-engine --bin flowsurface-indicator -- test data/indicators/sources --namespace core
cargo run --locked -p flowsurface-indicator-engine --bin flowsurface-indicator -- test indicators/sources --namespace catalog
```

- CI uses `RUSTFLAGS="-D warnings"`; use the equivalent environment setting for
  the active shell when reproducing CI.
- If a required check cannot run or fails, report the command and reason and
  clearly identify the PR as not fully validated.

## Running the application

- Launch the desktop application only when the user explicitly requests it.
  Compilation and automated tests do not imply permission to launch the GUI.
- For a requested manual run, build the requested branch or commit and verify
  that the launched executable corresponds to those sources.
- Prefer an isolated runtime configuration through `FLOWSURFACE_DATA_PATH` so a
  test run does not modify the user's usual saved layouts and settings.
- Report what was actually verified, including relevant runtime limitations.

## Reporting results

- Summarize what changed, which checks passed, and any remaining failures or
  limitations.
- When work is pushed, provide the branch, commit, or PR link as appropriate.
- Distinguish automated validation from manual GUI testing and live API checks.
