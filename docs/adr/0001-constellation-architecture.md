# ADR-0001: Constellation architecture — one API hub, many thin clients

- **Status**: accepted (inherited from the project template)
- **Date**: 2026-08-19

## Context

The project is a personal macOS tool with several ways in: a GUI app, a CLI
for scripting, and an MCP server so AI agents can drive it. All of them need
the same data and the same invariants. The tempting shortcut — each client
links the core crate and opens the SQLite file — creates N writers, N copies
of validation, and interop drift between them.

Other projects by the same author, built from the same template, converged on
the same answer independently.

## Decision

We will run a single Rust API server (`banshee-api`) as the only process
that opens the database. Every client — CLI, MCP server, Swift app — speaks
HTTP to it, authenticated by a locally generated key file. The Swift app
supervises the bundled server binary and discovers its port from the
server's stdout announcement.

```
┌─────────────────────────────────────────────────────────────┐
│  SwiftUI App (Banshee)                                    │
│  ┌────────────────────────────────────────────────────┐     │
│  │  DaemonService (client; ADR-0004 removed supervision)│    │
│  │  - Launches bundled API binary as child process    │     │
│  │  - Parses port announcement from stdout            │     │
│  │  - Handles "server didn't start" as first-class    │     │
│  └──────────────────────┬─────────────────────────────┘     │
│                         │ supervises (launch/terminate)     │
│                         ↓                                    │
│  ┌────────────────────────────────────────────────────┐     │
│  │  APIClient (Swift)                                 │     │
│  │  HTTP client to 127.0.0.1:<announced-port>        │     │
│  └──────────────────────┬─────────────────────────────┘     │
└─────────────────────────┼───────────────────────────────────┘
                          │
          ┌───────────────┼───────────────┐
          │   HTTP + X-Api-Key (loopback) │
          │   Auth: key file (0600)       │
          └───────────────┬───────────────┘
                          │
        ┌─────────────────┼─────────────────┐
        │                 │                 │
   ┌────┴─────┐     ┌─────┴──────┐    ┌────┴──────┐
   │   CLI    │     │ MCP server │    │ SwiftUI   │
   │ (Rust)   │     │  (Rust)    │    │APIClient  │
   └────┬─────┘     └─────┬──────┘    └────┬──────┘
        │                 │                 │
        └─────────────────┼─────────────────┘
                          ↓
              ┌───────────────────────┐
              │  banshee-api       │
              │  (Axum/Rust)          │
              │  Binds: 127.0.0.1:*   │
              │  Port ladder: +0..+9  │
              │  Auth: X-Api-Key      │
              └───────────┬───────────┘
                          │ ONLY database writer
                          ↓
              ┌───────────────────────┐
              │  SQLite               │
              │  banshee.db        │
              │  Forward-only schema  │
              └───────────────────────┘
```

**Key boundaries:**
- **Supervision boundary**: SwiftUI app → API binary (parent/child process)
- **Network boundary**: All clients → API via HTTP (loopback only)
- **Trust boundary**: X-Api-Key from key file (keeps other local users out)
- **Data boundary**: API → SQLite (nothing else touches the DB)

The wire format between them is a contract pinned by the fixtures in
`tests/fixtures/`, which the Rust tests, the Swift tests, and the CLI-vs-HTTP-vs-MCP
parity harness (`tests/e2e/run-e2e.sh`) all consume from the same bytes.

We will build the Swift side as a SwiftPM package (no `.xcodeproj`), with
`scripts/build-app.sh` assembling the `.app` bundle. pbxproj files demand
hand-edited entries per test file and do not survive a project rename cleanly;
SwiftPM makes `swift test` the test entry point and a rename a text edit.

## Consequences

- One writer: SQLite corruption class eliminated; auth and invariants live
  in one place.
- Clients are trivially replaceable and new ones (TUI, web) are additive.
- Cost: a local HTTP hop and a supervised child process; the app must handle
  "server didn't start" as a first-class state (it does — with the stderr
  log file surfaced in the error).
- Escape hatch for the SwiftPM choice: if asset catalogs / Sparkle demand an
  Xcode project later, generate one with `xcodegen` from a spec file rather
  than hand-maintaining pbxproj; `swift test` remains the test entry point.
