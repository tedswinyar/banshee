// The single source of truth for the app's release version.
// scripts/build-app.sh reads it; scripts/release.sh enforces that this and
// rust/Cargo.toml agree before tagging.
// (VERSIONING.md documents the policy.)

enum Version {
    static let marketing = "0.1.4"
}
