# Monogram Desktop

Read `../AGENTS.md` before working here. Mobile and desktop must reach **100%
feature parity**, including protocol, security and persistence behavior.
The mobile reference repository is `../mobile/`; keep the feature parity ledger
in `docs/FEATURE_PARITY.md` current. A fixture is not an implemented feature.

## Commit messages: Conventional Commits (required)

Every commit that lands on `main` **must** follow
[Conventional Commits](https://www.conventionalcommits.org/). release-please
(`.github/workflows/release.yml`) reads these messages to decide the next version
and to write the changelog. A commit without a valid prefix is ignored by it, and
the AppImage is only built when a release-please release is created.

Format: `type(optional scope): short imperative summary`

| Type | Use for | Version bump (0.x) |
|------|---------|--------------------|
| `feat` | New user-visible behavior | minor |
| `fix` | Bug fix | patch |
| `perf` | Performance improvement | patch |
| `revert` | Reverting an earlier commit | patch |
| `docs`, `style`, `refactor`, `test`, `build`, `ci`, `chore` | No user-visible change | none (not released alone) |

Rules:
- Lowercase type, then `:` and a space. Summary in the imperative, no trailing
  period, ideally 72 characters or fewer.
- Breaking changes: add `!` after the type (`feat!: ...`) and/or a
  `BREAKING CHANGE:` footer explaining the migration.
- One logical change per commit. Do not bundle unrelated work (for example a
  renderer swap and a UI feature) in one commit.
- Never hand-edit `CHANGELOG.md`, the version in `Cargo.toml`, or
  `.release-please-manifest.json`; the release PR does that.
- Pull request titles that will be squash-merged must also be valid Conventional
  Commit messages, since the title becomes the commit.
- Use a scope when it helps, such as `fix(sticker): ...`, `feat(composer): ...`,
  `ci(appimage): ...`.

Examples:
```
feat(composer): add opt-in Discord-style emoji completion
fix(math): reject formulas that render larger than the image limit
ci: cache Rust dependencies for AppImage builds
refactor!: replace the Java math helper with a Rust renderer
```
