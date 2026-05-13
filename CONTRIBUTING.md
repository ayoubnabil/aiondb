# Contributing to AionDB

AionDB core is licensed under the Business Source License 1.1 (`BUSL-1.1`)
with a future Apache License 2.0 change license. The authoritative BSL text is
in `LICENSE`; commercial terms require a separate written agreement.

This document explains the contribution rules for patches, documentation,
tests, examples, and other material submitted to this repository.

This document is operational guidance for the repository. It is not legal
advice.

## Contribution license

By submitting a contribution to AionDB, you agree that the contribution is
licensed under the same project terms as the rest of the repository:

- Business Source License 1.1 (`BUSL-1.1`), with the Apache License 2.0 change
  license stated in `LICENSE`.

This applies unless a contribution is explicitly marked in writing as not being
offered for inclusion.

Do not submit code, documentation, tests, data, generated output, or other
material unless you have the right to submit it under these terms.

Because AionDB also offers commercial licenses, larger or recurring
contributors may be asked to sign a separate contributor agreement before their
patches are accepted.

## Ownership of submissions

Only submit material that is one of the following:

- created by you;
- created by your employer or organization with permission to submit it;
- derived from material whose license permits submission under AionDB's terms;
- explicitly authorized by the original author for inclusion.

Do not submit copied code from another project unless the license is compatible
and attribution requirements are preserved.

## Developer Certificate of Origin

AionDB uses the Developer Certificate of Origin 1.1 sign-off process. By
signing off a commit, you certify that you wrote the contribution or otherwise
have the right to submit it to this project under the project license.

Add a sign-off line to every commit:

```text
Signed-off-by: Your Name <you@example.com>
```

Git can add this automatically:

```bash
git commit -s
```

The DCO 1.1 text is available from the Linux Foundation's DCO project and
mirrors such as the Eclipse Foundation DCO page.

Pull requests may be rejected if commits are not signed off.

## Scope

Useful contributions include:

- reduced SQL compatibility repros;
- small correctness tests;
- documentation fixes with exact commands;
- benchmark harness improvements that preserve reproducibility;
- bug reports with commit hash, SQL text, expected result, and actual result.

For larger features, open an issue first. AionDB is still alpha, so broad
changes should be discussed before implementation work begins.

## Style

- Keep changes narrow.
- Prefer explicit errors over silent fallback behavior.
- Add tests for behavior changes.
- Do not mix formatting-only edits with feature work.
- Do not add generated artifacts or local benchmark output to source control.
- Document limitations near the feature they affect.

## Pull request checklist

Before opening a pull request, check:

- the commits are signed off;
- the change has a focused purpose;
- tests or fixtures cover behavior changes;
- documentation is updated when public behavior changes;
- benchmark claims include reproduction details;
- no secrets, local data directories, or generated reports are included.

## Security and private material

Do not submit secrets, production data, private keys, credentials, customer
data, proprietary third-party code, or benchmark datasets that cannot be
redistributed under the project terms.

If you find a security issue, do not publish exploit details in a public issue
before a disclosure path exists for the project.

## Benchmark claims

Benchmark contributions must include:

- exact command;
- dataset size;
- commit hash;
- hardware and OS details;
- durability settings;
- raw output.

Performance summaries without reproduction details are not actionable.

## Not a support contract

Submitting an issue, pull request, or benchmark result does not create a
support obligation. Commercial support and commercial licensing require a
separate agreement with the licensor.
