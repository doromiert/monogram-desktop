# Monogram Desktop

Read `../AGENTS.md` before working here. Mobile and desktop must reach **100%
feature parity**, including protocol, security and persistence behavior.
The mobile reference repository is `../mobile/`; keep the feature parity ledger
in `docs/FEATURE_PARITY.md` current. A fixture is not an implemented feature.

For GTK/libadwaita UI changes, follow `docs/GNOME_UI_GUIDELINES.md`.

## Pull requests after a push (required)

Applies only to commits that were actually pushed, never local ones.

**Gates.** Do nothing if `gh` is not installed (`command -v gh`). Check the
account's permission once (`gh repo view --json viewerPermission`); it only decides
whether merging is possible:
- `READ` (or `TRIAGE`) is enough to comment on a pull request and to edit the
  title and description of your own PR. Do not skip those steps, and do not tell
  the user they are impossible.
- Merging needs `WRITE`, `MAINTAIN` or `ADMIN`. Without one of them, never offer
  or attempt a merge; offer **comment** only.

**API limits.** Fetch the open PR list once per push (`gh pr list --state open`)
and reuse it; no polling or loops. At most one comment or one edit per PR per
push. If unsure a PR matches, skip it.

Then, for each open PR a pushed commit relates to:

**Someone else's PR** (commit author is not the PR author) that already tracks
the feature or fix:
1. Alert the user with the PR link and the commit hash link, and ask whether to
   **comment**, **merge** or **both** (only **comment** without merge
   permission). Ask once per push, listing every affected PR. Do nothing until
   they answer.
2. Skip a PR whose comments already name that hash, so re-pushes never repeat.
3. Comment with the 7+ character hash, then merge only the PRs the user chose:
   - One item: `I fixed **<feature or bug fix>** in <commit hash>`
   - Several:
     ```
     I fixed the following feature (or bug fixes):
     **<feature or bug fix>** in <commit hash>
     ```

**Your own PR** (commit author is the PR author) on the pushed branch
(`--head <branch>`): never comment. If the push adds something the title or body
does not cover (feature, fix, behavior or breaking change):
1. Edit only when something is missing. Extend the body with a bullet or section;
   never drop text the author wrote.
2. Keep the title a valid Conventional Commit; change it only when the PR's main
   purpose changed.
3. Tell the user what changed.

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
