# Contributing

This is a personal project; contributions are by arrangement rather
than an open PR queue. If something here is useful to you and you want to
change it, open an issue (or reach the author directly) before writing code.

**Community standards**: This project follows the [Contributor Covenant v2.1](https://www.contributor-covenant.org/version/2/1/code_of_conduct/)
Code of Conduct (see [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)). Be
respectful, assume good intent.

If we have agreed on a change:

- `./scripts/verify.sh` green is the bar — the pre-push hook enforces it.
- The testing standard is structural, not a coverage percentage:
  - **Mutation-proof.** Make two or more deliberate single-line production
    mutations per file under test and name the test that caught each
    (`scripts/mutate.sh` refuses to report a green run it cannot vouch for).
  - **Mock only at boundaries** (`MockAPIClient` is the one approved mock); drive
    the real parsers, validators and mappers.
  - **Parser tests start from raw bytes** (`tests/fixtures/`), and every error
    branch is reached.
- This tree is public and verify keeps it that way: `scripts/lib/residue-gate.py`
  refuses home paths, LAN addresses, e-mail addresses and the maintainer's private
  material in any tracked file (ADR-0011). Write around them; do not exempt them.
- `docs/adding-a-field.md` is the touch-point checklist for any domain change.
- Wire-format changes additionally require updating `tests/fixtures/` and
  `docs/wire-format.md` in the same push.
