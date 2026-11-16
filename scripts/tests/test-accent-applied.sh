#!/usr/bin/env bash
set -u

# test-accent-applied.sh — holds the colour rules to the SOURCE (banshee-v54.2).
#
# `Palette.accent`'s doc comment said "applied at the app root via .tint" for months
# while no `.tint(` call existed anywhere in swift/Sources: controls drew in the
# system accent, the one real action drew in link blue, and inert advice drew in
# purple — so purple looked clickable and was not. An outside design review found it;
# no test could, because the app target has no test suite and a tint is not a value
# any unit test reads.
#
# The rules, checked as text because that is the only place they exist:
#   1. The accent is applied at all THREE view roots: the detail window and Settings
#      (BansheeApp.swift) and the AppKit-hosted popover (MenuBarController.swift),
#      which inherits no SwiftUI scene's tint.
#   2. No `.buttonStyle(.link)`: it draws the system's link blue whatever the tint
#      is, which is how the app came to have two interactive colours.
#   3. An action label is never drawn as bare accent TEXT: advice goes through
#      `adviceLine` in secondary text; only a Button may carry the accent.
#   4. The band colours are for band dots and nothing else (banshee-v54.3): the
#      `Palette.band*` tokens are named in exactly one place, `DimensionRow.color`,
#      and the History chart does not borrow the warning colour for a line that is
#      not a band.
#
# A text check can rot into a check of nothing, so each rule is also run against a
# copy of the sources with that rule broken, and must FAIL there.

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PASS=0
FAIL=0
t() { local d="$1"; shift; if "$@" >/dev/null 2>&1; then PASS=$((PASS+1)); else FAIL=$((FAIL+1)); echo "  ✗ $d" >&2; fi; }

# check <sources-dir> — exit 0 iff every rule holds. Prints the rule that failed.
check() {
  local src="$1" app="$1/Banshee/BansheeApp.swift" bar="$1/Banshee/MenuBarController.swift"
  local n
  [ -f "$app" ] && [ -f "$bar" ] || { echo "missing BansheeApp.swift or MenuBarController.swift"; return 2; }

  n="$(command grep -c -F '.tint(Palette.accent)' "$app")"
  [ "$n" -ge 2 ] || { echo "rule 1: BansheeApp.swift applies the accent $n time(s); the window and Settings each need it"; return 1; }
  command grep -q -F '.tint(Palette.accent)' "$bar" \
    || { echo "rule 1: the popover's hosting root does not apply the accent"; return 1; }

  if command grep -R -q -F 'buttonStyle(.link)' "$src/Banshee"; then
    echo "rule 2: .buttonStyle(.link) draws link blue, not the accent"; return 1
  fi

  # Rule 3: a `Text(finding.actionLabel)` followed within three lines by the accent,
  # outside a Button's `label:` closure. The popover's disk button is the one legal
  # shape, and it sits under a `} label: {` line.
  if awk '
      /label: \{/ { in_label = 4 }
      /Text\(finding\.actionLabel\)/ { if (in_label <= 0) watch = 3 }
      watch > 0 && /foregroundStyle\(Palette\.accent\)/ { bad = 1 }
      { if (watch > 0) watch--; if (in_label > 0) in_label-- }
      END { exit bad ? 0 : 1 }
    ' "$src"/Banshee/*.swift; then
    echo "rule 3: an action label is drawn as accent text outside a Button"; return 1
  fi

  n="$(command grep -R -E -l 'Palette\.band(Green|Amber|Red)' "$src/Banshee" | command grep -v -c '/ContentView.swift$')"
  [ "$n" -eq 0 ] || { echo "rule 4: a band colour is used outside DimensionRow.color"; return 1; }
  n="$(command grep -c -E 'Palette\.band(Green|Amber|Red)' "$src/Banshee/ContentView.swift")"
  [ "$n" -eq 3 ] || { echo "rule 4: ContentView.swift names band colours $n times; DimensionRow.color names each once"; return 1; }
  if command grep -q -F 'Palette.warning' "$src/Banshee/HistoryView.swift"; then
    echo "rule 4: the History chart draws a line in the warning colour"; return 1
  fi
  return 0
}

SRC="$ROOT/swift/Sources"

# 1. The real tree obeys all three rules.
t "the real sources obey the colour rules" check "$SRC"

# 2. Each rule fails on a copy with that rule broken. Edits are asserted to have
#    changed the file: a sed that matches nothing would make these pass for nothing.
WORK="$(mktemp -d /tmp/banshee-accent-test.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT

fresh() { rm -rf "$WORK/src"; mkdir -p "$WORK/src"; /bin/cp -Rf "$SRC/Banshee" "$WORK/src/Banshee"; }
# mutate <file> <sed-expr> — apply, and fail loudly if the file did not change.
mutate() {
  local f="$WORK/src/Banshee/$1" before after
  before="$(shasum "$f")"
  sed -i '' -e "$2" "$f"
  after="$(shasum "$f")"
  [ "$before" != "$after" ]
}
breaks() { ! check "$WORK/src"; }

fresh
t "mutation applied: popover tint removed" mutate MenuBarController.swift 's/\.environment(model)\.tint(Palette\.accent)/.environment(model)/'
t "rule 1 catches a popover with no tint" breaks

fresh
t "mutation applied: Settings tint removed" mutate BansheeApp.swift '/SettingsView(preferences: preferences)/{n;d;}'
t "rule 1 catches a Settings scene with no tint" breaks

fresh
t "mutation applied: a link-style button" mutate ContentView.swift 's/\.buttonStyle(\.plain)/.buttonStyle(.link)/'
t "rule 2 catches a link-style button" breaks

fresh
t "mutation applied: advice drawn in the accent again" mutate ContentView.swift 's/Text(finding\.adviceLine)/Text(finding.actionLabel)/'
t "mutation applied: and coloured with the accent" mutate ContentView.swift '/Text(finding\.actionLabel)/{n;n;s/Palette\.textSecondary/Palette.accent/;}'
t "rule 3 catches accent text that is not a button" breaks

fresh
t "mutation applied: the max line borrows the warning colour" mutate HistoryView.swift 's/"max": Palette\.textSecondary/"max": Palette.warning/'
t "rule 4 catches a chart line in the warning colour" breaks

fresh
t "mutation applied: a band colour used in the reap sheet" mutate ReapSheet.swift 's/Circle()\.fill(Palette\.textPrimary)/Circle().fill(Palette.bandAmber)/'
t "rule 4 catches a band colour outside the band dot" breaks

echo "test-accent-applied: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
