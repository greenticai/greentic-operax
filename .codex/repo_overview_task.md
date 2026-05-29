# Repo Overview Maintenance

Maintain `.codex/repo_overview.md` as a current machine- and human-readable
summary of this repository.

## Required Routine

1. Scan the project structure and identify top-level components, crates,
   packages, workflows, scripts, examples, and `.codex` planning files.
2. Inspect main entrypoints, public APIs, tests, examples, and docs to infer
   what is actually implemented.
3. Search for TODO/WIP/stub markers including `TODO`, `FIXME`, `XXX`, `HACK`,
   `NOTE`, `BROKEN`, `TEMP`, `unimplemented!`, and `todo!`.
4. Run obvious non-destructive build and test commands, especially
   `ci/local_check.sh` when present.
5. Fully refresh `.codex/repo_overview.md`; do not append stale or conflicting
   information.

## Required Headings

- `# Repository Overview`
- `## 1. High-Level Purpose`
- `## 2. Main Components and Functionality`
- `## 3. Work In Progress, TODOs, and Stubs`
- `## 4. Broken, Failing, or Conflicting Areas`
- `## 5. Notes for Future Work`

