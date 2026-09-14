### Swift Conventions

When working on Swift code in this project:

- `@MainActor` for view models and UI-related classes; `async/await` over
  completion handlers.
- **Design tokens only** in views: `Spacing.*`, `Palette.*`, `Typography.*`,
  `Radius.*` from DesignKit. Raw point values and `Color.red`-style literals
  in view code are review findings.
- All wire types decode/encode through `Wire.decoder()` / `Wire.encoder()`.
  A bare `JSONDecoder()` for a wire type silently diverges on dates.
- Model property names ARE the wire keys (camelCase, no CodingKeys remapping).
  If you need a CodingKeys enum, the wire format probably changed — update
  the fixtures and the spec first.
- Mock ONLY at the `APIClientProtocol` boundary (`MockAPIClient`). Never mock
  URLSession, views, or the layer under test.
- Accessibility: every interactive element gets a sensible default via system
  controls; custom controls need `.accessibilityLabel`. Test VoiceOver on any
  new sheet or custom row before calling it done.
- Never index an array unguarded inside a test assertion — assert the whole
  array or `XCTUnwrap` (CLAUDE.md → Gotchas has the war story).
