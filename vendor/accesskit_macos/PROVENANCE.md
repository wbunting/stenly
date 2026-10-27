# Provenance of this vendored crate

- Upstream: <https://github.com/AccessKit/accesskit>, directory `platforms/macos`
- Version: `accesskit_macos` 0.26.3 (crates.io)
- Upstream commit: `c88605b96d04431f9c3c792464a0f2f253480e94`
  (tag `accesskit_macos-v0.26.3`; matches the crate's `.cargo_vcs_info.json`)
- License: `MIT OR Apache-2.0`, at your option. `src/node.rs` is additionally
  derived from Chromium's accessibility code under the BSD-style Chromium
  license.

The published crate archive omits the license files, so `LICENSE-MIT`,
`LICENSE-APACHE` and `LICENSE.chromium` were copied unmodified from the
repository root at the commit above. These notices apply to every file in this
directory; Stenly's root MIT license does not replace them.

## Local modifications

- `src/subclass.rs`: backport of AccessKit PR #822 (issue #823). Do not reset
  the content view's class when KVO has subclassed it on top of ours, which
  otherwise aborts the app on macOS 26 when a window closes. Marked with a
  "Stenly patch" comment. Remove this vendored copy once the dependency
  graph uses an AccessKit release that includes #822.
- Added this `PROVENANCE.md` and the three license files above.

All other files are identical to the 0.26.3 crate.
