# Monogram Desktop

Read `../AGENTS.md` before working here. Mobile and desktop must reach **100%
feature parity**, including protocol, security and persistence behavior.
The mobile reference repository is `../mobile/`; keep the feature parity ledger
in `docs/FEATURE_PARITY.md` current. A fixture is not an implemented feature.

For GTK/libadwaita UI changes, follow `docs/GNOME_UI_GUIDELINES.md`.

## Pull request comments for pushed fixes (required)

After a push that lands a feature or bug fix an open PR already tracks:

1. Do nothing if `gh` is not installed (`command -v gh`). Otherwise only act if
   the authenticated account can write to and merge PRs in the target repo
   (check `gh repo view --json viewerPermission`; `WRITE`, `MAINTAIN` or
   `ADMIN`). If either check fails, do nothing.
2. Only for commits that were actually pushed, never local or unpushed ones.
   Match each pushed commit against the open PRs (`gh pr list --state open`) by
   the issue, feature or bug it addresses. Skip commits that match no open PR.
   Also skip a PR whose author is the author of the pushed commit (compare the
   commit author with the PR author's login); that is the user's own work, so
   do nothing for it.
3. Before commenting, alert the user that the feature or fix already exists in
   the open PR (give the PR link and the pushed commit hash link) and ask
   whether to **comment**, **merge**, or **both**. Do not comment or merge
   until they answer. Ask once per push, listing all affected PRs together.
4. On approval, comment on each chosen PR. Use the full or 7+ character hash
   from the pushed commit. Merge only the PRs the user said to merge.
   - One feature or fix: `I fixed **<feature or bug fix>** in <commit hash>`
   - Several: one line per item under a heading, for example
     ```
     I fixed the following feature (or bug fixes):
     **<feature or bug fix>** in <commit hash>
     **<feature or bug fix>** in <commit hash>
     ```
5. Keep GitHub API usage low:
   - One comment per PR per push, listing every matching commit hash together.
   - Fetch the open PR list once per push and reuse it; do not poll or loop.
   - Before commenting, skip a PR whose existing comments already name that
     hash, so re-pushes and force-pushes never repeat a comment.
   - Never comment in bulk on unrelated PRs; if unsure a PR matches, skip it.

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
