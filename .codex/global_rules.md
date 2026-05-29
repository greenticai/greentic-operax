GLOBAL RULE - REPO OVERVIEW, CI, AND REUSE OF GREENTIC REPOS

For THIS REPOSITORY, you must always:

1. Maintain `.codex/repo_overview.md` using `.codex/repo_overview_task.md`
   before starting PR-style work and after finishing it.
2. Run `ci/local_check.sh` at the end of implementation work once it exists,
   or explain precisely why it cannot be run yet.
3. Prefer existing Greentic crates, contracts, and sibling repo patterns over
   redefining shared concepts locally.

Treat these as prerequisites and finalisation steps for all implementation work
in this repo.

## Reuse-first policy

Before adding core types, interfaces, schemas, scripts, workflows, or runtime
contracts, check the nearby Greentic repos for an existing pattern:

- `../greentic-sorx` for Rust workspace shape, runtime HTTP discipline,
  local checks, i18n, coverage, release, and `.codex` history.
- `../greentic-operala` for OperaLa handoff metadata and demo artifact shape.
- `../greentic-sorla` for SoRLa source contracts and generated SORX handoff
  metadata.
- shared Greentic crates for interfaces, secrets, OAuth, messaging, events,
  distributor resolution, and i18n.

Do not duplicate cross-repo models unless there is a clear documented reason.

## Finalisation

Every PR-style implementation should finish with:

- `.codex/repo_overview.md` refreshed.
- `bash ci/local_check.sh` run when available.
- A concise note about any skipped check and why.

