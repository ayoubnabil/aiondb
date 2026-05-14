# Third-Party Licenses

This file summarizes notable third-party material in this repository. It is not
a replacement for the original license files shipped with vendored components.

## AionDB Studio / pgweb

`integrations/aiondb-studio/` is based on pgweb.

- Upstream: <https://github.com/sosedoff/pgweb>
- License: MIT
- License file: `integrations/aiondb-studio/LICENSE`

The AionDB-specific changes in this repository follow AionDB's project license,
but upstream pgweb code remains under its original MIT license.

## AionDB Studio bundled browser assets

`integrations/aiondb-studio/static/` contains browser assets inherited from
pgweb, including jQuery, Bootstrap, Bootstrap context menu, Font Awesome, and
Ace editor assets.

For redistribution hygiene, see
`integrations/aiondb-studio/THIRD_PARTY_NOTICES.md`, which maps the vendored
files we ship to the upstream notices we verified in this repository.

## Test corpora and fixtures

Some test corpora, fixtures, and benchmark inputs remain under their upstream
licenses. Notable examples:

- The Cypher Technology Compatibility Kit (TCK) under `testing/cypher-tck/`
  is Apache License 2.0 with upstream copyright notices preserved in the
  feature files and a local `NOTICE` file in the subtree.
- Other vendored test data preserves its upstream license and attribution.

## Rust and Go dependencies

Rust and Go package dependencies are licensed by their respective authors. Use
the lockfiles and dependency tooling in this repository to generate a complete
dependency inventory for release artifacts.
