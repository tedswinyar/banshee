#!/usr/bin/env bash
set -u

# test-version-alignment.sh — tests OF scripts/check-version-alignment.sh, plus the
# one assertion that matters between releases: the REAL tree is aligned at HEAD.
#
# The checker exists because release.sh gated Version.swift alone and four releases
# shipped a daemon whose /health said "0.1.0" (banshee-87l.15). A checker is only
# worth having if it FAILS when the two files disagree, so every case below is driven
# against fixtures in a temp dir (BANSHEE_VERSION_SWIFT / BANSHEE_CARGO_TOML), in
# both directions. The last case runs it against the repo itself so `verify` catches a
# bump that touched one file and not the other before release day.

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
CHECK="$SCRIPT_DIR/check-version-alignment.sh"
PASS=0
FAIL=0

t() {
  local desc="$1"; shift
  if "$@" >/dev/null 2>&1; then PASS=$((PASS + 1)); else
    FAIL=$((FAIL + 1)); echo "  ✗ $desc" >&2; fi
}
t_fails() {
  local desc="$1"; shift
  if "$@" >/dev/null 2>&1; then
    FAIL=$((FAIL + 1)); echo "  ✗ $desc (passed and should not have)" >&2
  else PASS=$((PASS + 1)); fi
}

WORK="$(mktemp -d /tmp/banshee-version-test.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT

# Fixture writers. The Cargo fixture deliberately carries a DEPENDENCY `version =`
# line and a second table AFTER [workspace.package], so a checker that grabbed the
# first or last `version =` in the file would read the wrong one.
swift_fixture() { # path version
  printf 'enum Version {\n    static let marketing = "%s"\n    static let build = 7\n}\n' "$2" > "$1"
}
cargo_fixture() { # path version
  cat > "$1" <<CARGO
[workspace]
members = ["a"]

[workspace.package]
version = "$2"
edition = "2024"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }

[profile.release]
version = "9.9.9"
CARGO
}

run() { # swift cargo [expected]
  env BANSHEE_VERSION_SWIFT="$1" BANSHEE_CARGO_TOML="$2" "$CHECK" "${3-}"
}

S="$WORK/Version.swift"; C="$WORK/Cargo.toml"

# 1. Both say the same thing → pass (a checker that always fails is useless).
swift_fixture "$S" 1.2.3; cargo_fixture "$C" 1.2.3
t "aligned files pass" run "$S" "$C"

# 2. …and pass when the release argument agrees too.
t "aligned files pass against a matching release version" run "$S" "$C" 1.2.3

# 3. Cargo lags the app → FAIL. This is the exact shape that shipped: Version.swift
#    bumped, Cargo.toml forgotten, daemon reports the old number.
swift_fixture "$S" 1.2.4; cargo_fixture "$C" 1.2.3
t_fails "the checker fails when Cargo.toml lags Version.swift" run "$S" "$C"

# 4. The other direction: app lags Cargo → FAIL.
swift_fixture "$S" 1.2.3; cargo_fixture "$C" 1.2.4
t_fails "the checker fails when Version.swift lags Cargo.toml" run "$S" "$C"

# 5. Files agree with each other but not with the release argument → FAIL.
swift_fixture "$S" 1.2.3; cargo_fixture "$C" 1.2.3
t_fails "the checker fails when both files disagree with the release version" run "$S" "$C" 1.3.0

# 6. The Cargo fixture's decoys: a checker reading the FIRST `version =` gets the
#    dependency's "1"; reading the LAST gets "9.9.9". Only the section-scoped read
#    returns 1.2.3, so a case where the decoys would agree with Swift and the real
#    line would not must FAIL.
swift_fixture "$S" 9.9.9
t_fails "the checker reads [workspace.package], not the last version= line" run "$S" "$C"
swift_fixture "$S" 1
t_fails "the checker reads [workspace.package], not the first version= line" run "$S" "$C"

# 7. A Version.swift with no marketing line is a broken input, not "aligned".
printf 'enum Version {}\n' > "$S"; cargo_fixture "$C" 1.2.3
t_fails "a Version.swift without a marketing line fails" run "$S" "$C"

# 8. A Cargo.toml with no [workspace.package] version is a broken input too.
swift_fixture "$S" 1.2.3; printf '[workspace]\nmembers = ["a"]\n' > "$C"
t_fails "a Cargo.toml without a workspace version fails" run "$S" "$C"

# 9. THE REAL TREE. Version.swift and rust/Cargo.toml at HEAD must agree — this is
#    what turns a forgotten bump into a red verify instead of a wrong /health.
t "the repo's own Version.swift and Cargo.toml agree" "$CHECK"

echo "test-version-alignment: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
