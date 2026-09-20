#!/usr/bin/env python3
"""Refuse a committed file — or commit message — that carries private residue.

Banshee is ONE public repository with a nested, gitignored `private/` notebook (ADR-0011).
A gitignore is a promise, not a mechanism: `git add -f`, a rename outside the pattern, or a
hostname pasted into a public doc all land in the public tree with no error. This gate
runs in the scripts suite on every verify, over every tracked file AND every commit message
reachable from HEAD, and fails the gate when it finds:

  * a PATH that belongs in the notebook, tracked anyway — anything under `private/`,
    the session handoff, the issue-tracker export, the research notes, the template
    lineage, a release identity file;
  * a home path into a real user's directory (`/Users/<name>` other than the documented
    `example` and `builder` users, `~/Documents`, `~/Code`…);
  * an RFC 1918 address (a home-LAN machine, not a loopback or a documentation address);
  * an e-mail address other than a GitHub noreply or an `example.` one;
  * anything matching the OPTIONAL private denylist, `private/residue-denylist.txt`
    (one regular expression per line, `#` comments). Organisation, vendor and personal
    names, the build host: a list of those IS a leak, so it never enters the tree. When
    the file is present (the maintainer's checkout) the gate reads it; when it is absent
    (a fresh clone, the CI runner) the gate says so in its summary rather than
    pretending the check ran.

What is checked, and how thoroughly (an adversarial probe of an earlier version found
every one of these gaps, so each is now explicit):
  * file CONTENT, text or binary — a binary is decoded byte-for-byte (latin-1) and
    searched the same way, so a sentinel after a NUL is still found;
  * file NAMES — a denylisted word in a path is a leak too;
  * COMMIT MESSAGES of every commit reachable from HEAD (`--messages`), because a message
    that recounts what a commit removed publishes it anyway;
  * the gate's own source, its test and its mutation entries are exempt from the GENERIC
    prose checks only (they contain those patterns); the private denylist applies to them
    like everything else;
  * the two GENERATED attribution files (`THIRD-PARTY-NOTICES.html`, `sbom/*.cdx.json`,
    written by cargo-about / cargo-cyclonedx from crates.io metadata and license text)
    legitimately carry third-party authors' e-mail addresses, so in those files — and
    ONLY those — e-mail addresses are masked before every check. Everything else in them
    is gated in full: a home path (cargo-cyclonedx embeds `path+file:///Users/<user>/…`
    purls for workspace crates, so a release cut on a laptop names it), an RFC 1918
    address, a denylisted term outside an address. The first end-to-end release
    (v0.1.5, 2026-09-20) committed these files with 311 author addresses and turned
    verify red on main; license text must not be edited to remove them;
  * an unreadable tracked or explicitly named file is an ERROR (exit 2), never "clean".

Bead IDs are NOT gated: the tracker is private but `banshee-abc` is a harmless token,
and stripping IDs by shape once rewrote crate names (see the gotcha list on scrubbing).

Usage:
  residue-gate.py [--root DIR] [--denylist FILE|--no-denylist] [--messages] [FILE …]
    no FILE arguments → every path from `git ls-files` under --root (default: the
    repository this script lives in). Paths are repo-relative.
    --messages         → ALSO scan the messages of every commit reachable from HEAD.

Exit codes: 0 clean; 1 findings (each `path:line: reason` or `commit <sha>:line: reason`
on stderr, then a summary); 2 usage, an unreadable file, or an unreadable denylist.
"""

import argparse
import os
import re
import subprocess
import sys

# --- paths that must never be tracked in the public tree --------------------------------
FORBIDDEN_PATH_PREFIXES = (
    "private/",
    ".beads/",
    "docs/research/",
)
FORBIDDEN_PATHS = {
    "HANDOFF.md",
    "Config",
    ".public-mirror-base",
    "TEMPLATE.md",
    ".template-stamp.toml",
    "scripts/release.conf",
    "census.local.toml",
}

# --- prose that must not appear in a public file (generic; the specifics live in the
# private denylist) -----------------------------------------------------------------------
_RFC1918 = [
    r"\b10\.\d{1,3}\.\d{1,3}\.\d{1,3}\b",
    r"\b192\.168\.\d{1,3}\.\d{1,3}\b",
    r"\b172\.(?:1[6-9]|2\d|3[01])\.\d{1,3}\.\d{1,3}\b",
]
_HOME_PATHS = [
    # A path into a home directory is unusable by every reader and names a machine. The
    # two documented users are exempt: `example` (fixtures) and `builder` (the build
    # server's dedicated account, docs/build-server.md).
    r"/Users/(?!example\b|builder\b)[A-Za-z]",
    r"~/(?:Documents|Desktop|Code|bin)\b",
]
_EMAIL = [
    # Any address except a GitHub noreply (the maintainer's public identity), the
    # `git@github.com` of an ssh URL, or an example.* one. Fixture identities like
    # `t@x` have no TLD and do not match.
    r"[A-Za-z0-9._%+-]+@(?!users\.noreply\.github\.com\b|github\.com\b|example\.)[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}\b",
]
# The maintainer's own codenames, tools and hardware are NOT listed here: publishing
# that list is itself the leak (a second pre-publication review found the tree
# confirming one of them). They live in the private denylist with everything else.
FORBIDDEN_PROSE = _RFC1918 + _HOME_PATHS + _EMAIL

# Files that legitimately contain the GENERIC patterns: this gate, its test (which
# writes violating fixtures), and the mutation entries that quote this gate's source.
# The private denylist is NOT waived for them.
EXEMPT_FILES = {
    "scripts/lib/residue-gate.py",
    "scripts/tests/test-residue-gate.sh",
}
EXEMPT_PREFIXES = (".mutations/residue-gate-",)

# Generated third-party attribution: cargo-about's notices and cargo-cyclonedx's SBOMs.
# Regenerated by release.sh on every cut, so nothing hand-written survives in them; the
# class is deliberately narrow (exactly this file, exactly this directory + suffix) so a
# stray `sbom/notes.md` is gated like any other file.
ATTRIBUTION_FILES = {"THIRD-PARTY-NOTICES.html"}
ATTRIBUTION_DIR = "sbom/"
ATTRIBUTION_SUFFIX = ".cdx.json"
_EMAIL_RX = re.compile(_EMAIL[0], re.IGNORECASE)

DEFAULT_DENYLIST = os.path.join("private", "residue-denylist.txt")


class GateError(Exception):
    """A condition the gate cannot judge — reported as exit 2, never as clean."""


def repo_root_of_this_file():
    return os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))


def tracked_files(root):
    out = subprocess.run(
        ["git", "-C", root, "ls-files", "-z"], check=True, capture_output=True
    ).stdout
    return [p.decode("utf-8", "surrogateescape") for p in out.split(b"\0") if p]


def commit_messages(root):
    """[(sha, message)] for every commit reachable from HEAD, newest first."""
    out = subprocess.run(
        ["git", "-C", root, "log", "--format=%H%x00%B%x00", "HEAD"],
        check=True, capture_output=True,
    ).stdout.decode("utf-8", "surrogateescape")
    parts = out.split("\0")
    pairs = []
    for i in range(0, len(parts) - 1, 2):
        sha = parts[i].strip()
        if sha:
            pairs.append((sha, parts[i + 1]))
    return pairs


def load_denylist(path):
    """One regex per line; blank lines and `#` comments ignored. Returns compiled list."""
    patterns = []
    with open(path, encoding="utf-8") as fh:
        for n, line in enumerate(fh, 1):
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            try:
                patterns.append((line, re.compile(line, re.IGNORECASE)))
            except re.error as e:
                raise GateError("%s:%d: bad regex %r: %s" % (path, n, line, e))
    return patterns


def path_findings(path, prose, denylist):
    findings = []
    if path in FORBIDDEN_PATHS or path.startswith(FORBIDDEN_PATH_PREFIXES):
        findings.append((0, "path belongs in the private notebook, not the public tree"))
    # A file NAME can carry residue as easily as its content.
    for pat, rx in prose + denylist:
        if rx.search(path):
            findings.append((0, "file name matches " + pat))
    return findings


def is_exempt(path):
    return path in EXEMPT_FILES or path.startswith(EXEMPT_PREFIXES)


def is_attribution(path):
    return path in ATTRIBUTION_FILES or (
        path.startswith(ATTRIBUTION_DIR) and path.endswith(ATTRIBUTION_SUFFIX)
    )


def line_findings(lines, prose, denylist, where):
    """Findings over an iterable of (line_no, text). `where` labels binary content."""
    findings = []
    for n, line in lines:
        for pat, rx in prose:
            if rx.search(line):
                findings.append((n, where + "private residue: matches " + pat))
        for pat, rx in denylist:
            if rx.search(line):
                findings.append((n, where + "denylisted term: matches " + pat))
    return findings


def content_findings(path, data, prose, denylist):
    """Gate a file's bytes. Exempt files skip the generic prose checks only; generated
    attribution files have their e-mail addresses masked and are otherwise gated in full."""
    prose_here = [] if is_exempt(path) else prose
    if b"\0" in data[:8192]:
        # Binary: no line structure, but the bytes are still searched — a sentinel after a
        # NUL is exactly what an earlier probe slipped past.
        text = data.decode("latin-1")
        return line_findings([(0, text)], prose_here, denylist, "binary content: ")
    text = data.decode("utf-8", "surrogateescape")
    if is_attribution(path):
        # Third-party authors' addresses are the file's purpose, not residue. Mask them so
        # neither the generic e-mail check nor a denylisted term INSIDE an address fires;
        # the rest of the line (a purl with a home path, a stray word) is still searched.
        text = _EMAIL_RX.sub("<e-mail>", text)
    return line_findings(enumerate(text.split("\n"), 1), prose_here, denylist, "")


def read_bytes(root, path):
    full = os.path.join(root, path)
    try:
        with open(full, "rb") as fh:
            return fh.read()
    except OSError as e:
        raise GateError("cannot read %s: %s" % (path, e.strerror or e))


def main(argv):
    p = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    p.add_argument("--root", default=None, help="repository root (default: this script's repo)")
    p.add_argument("--denylist", default=None, help="regex-per-line file (default: private/residue-denylist.txt if present)")
    p.add_argument("--no-denylist", action="store_true", help="run only the generic checks")
    p.add_argument("--messages", action="store_true", help="also gate every commit message reachable from HEAD")
    p.add_argument("files", nargs="*", help="repo-relative paths (default: git ls-files)")
    args = p.parse_args(argv)

    root = os.path.abspath(args.root or repo_root_of_this_file())
    try:
        files = args.files or tracked_files(root)

        denylist, denylist_source = [], "none"
        if not args.no_denylist:
            cand = args.denylist or os.path.join(root, DEFAULT_DENYLIST)
            if os.path.isfile(cand):
                denylist = load_denylist(cand)
                denylist_source = "%s (%d patterns)" % (os.path.relpath(cand, root), len(denylist))
            elif args.denylist:
                raise GateError("denylist not found: %s" % cand)
            else:
                denylist_source = "none (%s absent — generic checks only)" % DEFAULT_DENYLIST

        prose = [(pat, re.compile(pat, re.IGNORECASE)) for pat in FORBIDDEN_PROSE]
        total = 0
        for path in files:
            found = path_findings(path, prose, denylist)
            found += content_findings(path, read_bytes(root, path), prose, denylist)
            for line, reason in found:
                print("%s:%d: %s" % (path, line, reason), file=sys.stderr)
            total += len(found)

        n_msgs = 0
        if args.messages:
            for sha, message in commit_messages(root):
                n_msgs += 1
                found = line_findings(enumerate(message.split("\n"), 1), prose, denylist, "")
                for line, reason in found:
                    print("commit %s:%d: %s" % (sha[:12], line, reason), file=sys.stderr)
                total += len(found)
    except GateError as e:
        print("residue-gate: ERROR: %s" % e, file=sys.stderr)
        return 2

    verdict = "clean" if total == 0 else "%d finding(s)" % total
    msgs = (", %d commit message(s)" % n_msgs) if args.messages else ""
    print("residue-gate: %d file(s)%s, denylist: %s — %s" % (len(files), msgs, denylist_source, verdict))
    return 1 if total else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
