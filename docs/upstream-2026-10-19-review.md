# Upstream review: 2026-10-19

Fetched upstream/main: `ee32572d54e01ba787d10e232a3c9d2d83408372`.
Integration base: `4fd14be` (develop). Backup: `backup/develop-before-upstream-20261019`.
This continues the selective integration described in [the previous review](upstream-0.8.10-integration.md); upstream ancestry is not marked as fully merged.

| Upstream commit | Decision | Reason |
| --- | --- | --- |
| `5bd5e69` | Retain the vendored optional catalog | Upstream moved it to flowsurface-rs/indicators. Flowdepth embeds sources from this directory for its integrated indicator browser; deleting them would break the fork. Keep the existing snapshot, licenses and catalog validation. |
| `c1f36d9` | Cherry-pick as `a1f591a` | Initialize the global quantity unit and new saved states consistently to quote currency. Existing saved preferences remain respected. |
| `25aa9c9` | Keep the fork font | This corrects horizontal bearings for upstream's redesigned glyph outlines. The fork retains different outlines and bearings. Direct replacement also drops codepoint U+F196 (plus-squared-alt); adopting that binary requires a separate font migration. |
| `5077a2f` | Defer multi-ticker Time & Sales | Changes tape storage, trade ordering, per-ticker quantities, stream subscriptions, the picker and settings across seven files. The fork still uses the older panel and pane architecture, so this needs a dedicated port with saved-layout and mixed-market validation. It cannot be directly cherry-picked onto the retained dashboard. |
| `ee32572` | Port as `a86fa19` | Show the actual multiplied price step instead of undoing the multiplier. Adapt upstream's moved pane/ladder paths and extend the same correction to the fork's Footprint and GPU heatmap controls. Ladder exposes its pending step immediately, before the next depth update. Remove unused unscale helpers. |

Both imported fixes preserve upstream author `akenshaw <akenshaw@pm.me>` and original author dates. The quantity fix includes the original SHA via cherry-pick -x; the Step port reuses the original commit message and authorship via git commit -C. Committer identity records the local integration.

All changes are made in the existing develop worktree at target/upstream-realign. The main worktree's uncommitted Hyperliquid/Paradex work stays on feat/hyperliquid-orderflow-depth. No remote push is part of this review.

## Validation

Passed on Windows with the pinned Rust 1.95.0 toolchain:

- cargo fmt --all -- --check
- cargo check --workspace --all-targets --all-features --locked
- cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
- cargo test --workspace --all-targets --all-features --locked: 419 passed, 0 failed, 11 ignored by the suite.
- git diff --check

The application was not manually exercised against live exchange feeds. Indicator source files and their runtime are unchanged by these fixes.
