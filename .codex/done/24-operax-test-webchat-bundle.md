# PR 24 — OperaX `test <pack>` WebChat bundle integration

## Goal

Complete the SORX-style WebChat test experience for `greentic-operax test
<pack>` on top of the manager runtime added in PR 23.

The finished command should prepare a local WebChat GUI bundle, start the
OperaX manager, inject live manager cards, and let a user paste or upload
single-item or multi-item JSON through WebChat for dry-run or explicit apply
processing.

## SORX Pattern To Follow

Use the current Rust implementation behind `greentic-sorx test`, especially
`../greentic-sorx/crates/greentic-sorx-cli/src/test_runtime.rs`.

Do not use `scripts/test_sorx.sh` as the design baseline; it is historical.

SORX currently:

- builds a temporary Greentic bundle with the messaging WebChat GUI provider;
- generates local runtime answers for memory-backed startup;
- starts the SORX HTTP runtime;
- waits for the manager dashboard card endpoint;
- injects live manager cards into the default app pack; and
- patches WebChat hooks so Adaptive Card submit/open actions call the live
  manager API.

OperaX should mirror that integration shape while targeting the PR 23 manager
routes under `/v1/operax/...`.

## CLI Shape

Extend:

```text
greentic-operax test <artifact>
  --tenant <TENANT>
  --team <TEAM>
  --sorx-url <URL>
  --operax-url <URL>
  --webchat-url <URL>
  --audit-dir <DIR>
  --bundle-dir <DIR>
  --answers <SETUP_ANSWERS>
  --locale <LOCALE>
  --force
  --no-start
```

Environment overrides:

```text
OPERAX_TEST_BUNDLE_DIR
OPERAX_TEST_SETUP_ANSWERS
OPERAX_TEST_TENANT
OPERAX_TEST_TEAM
OPERAX_TEST_LOCALE
OPERAX_TEST_NO_START
OPERAX_BIN
SORX_URL
```

Keep any shell helper as a thin wrapper around this CLI command only.

## WebChat Bundle Flow

Implement preparation in Rust, following the SORX test runtime:

1. Validate `<pack>` and resolve its absolute path.
2. Create a temp bundle workspace named `operax-manager-{pack_id}`.
3. Use the WebChat GUI provider:
   `oci://ghcr.io/greenticai/packs/messaging/messaging-webchat-gui:stable`.
4. Run setup non-interactively with generated or supplied answers.
5. Patch WebChat hooks to recognize:
   - `operax_manager_submit`
   - `operax_manager_open`
   - optional `operax_json_file_upload`
6. Start the OperaX manager HTTP runtime.
7. Wait for `/v1/operax/manager/cards/dashboard`.
8. Inject live cards into the default app pack for initial WebChat navigation.
9. Start the WebChat bundle unless `--no-start` is set.

Expected terminal output should mirror SORX:

```text
Preparing OperaX/WebChat test bundle
  OperaX pack:       ...
  bundle workspace:  ...
  manager card URL:  http://127.0.0.1:8797/v1/operax/manager/cards/dashboard
  WebChat URL:       http://127.0.0.1:8080/webchat
  SORX URL:          http://127.0.0.1:8787
  tenant/team:       demo-tenant / property-ops
```

## WebChat Hook Behavior

Patch the WebChat GUI so:

- card submit posts to `/v1/operax/manager/submit`;
- open actions fetch `/v1/operax/manager/cards/{target}`;
- returned cards are dispatched as incoming WebChat Adaptive Card activities;
- JSON file upload reads the selected file and submits its contents; and
- errors are rendered as WebChat text messages or error cards.

The first card should be an OperaX dashboard that can open the input card, run
dry-run processing, and show recent decision cards.

## Tests

Add tests equivalent to the current SORX coverage:

- `greentic-operax test examples/tenancy/handoff --no-start` creates a bundle
  workspace with WebChat provider and initial cards.
- WebChat hook patch contains the OperaX manager submit/open hooks.
- Local manager runtime is reachable from the generated bundle configuration.
- End-to-end dry-run through `/v1/operax/runs` emits the tenancy demo decisions.

Optional Playwright:

- start the `test` bundle;
- open `/webchat`;
- paste/upload `daily-transactions.json`;
- assert the decision card shows matched, partial payment, and unmatched rows.

## Acceptance Criteria

- `greentic-operax test <pack> --no-start` builds the local WebChat bundle and
  prints manager/WebChat URLs.
- `greentic-operax test <pack>` starts the manager runtime and WebChat bundle.
- WebChat can paste/upload JSON and receive OperaX decision cards.
- Adaptive Card actions use the live PR 23 manager API.
- CI/local checks cover no-start bundle preparation and WebChat hook patching.

## Implementation Notes

Implemented in `crates/operax-cli/src/test_runtime.rs`.

The test command now:

- resolves the OperaX artifact and prepares an `operax-manager-{pack_id}` bundle
  workspace;
- writes `greentic-bundle wizard apply` answers for the stable messaging
  WebChat GUI provider;
- runs `gtc setup` with generated or supplied setup answers;
- validates generated `.gtpack` files through `greentic-pack-lib` before
  patching them;
- patches WebChat hooks with OperaX submit/open handlers targeting the PR 23
  manager API;
- injects placeholder manager cards for `--no-start`;
- starts the OperaX manager server, waits for the dashboard endpoint, injects
  live manager cards, and then starts the WebChat bundle when `--no-start` is
  not set.

Verified:

- `cargo run --bin greentic-operax -- test --help`
- `cargo run --bin greentic-operax -- test examples/tenancy/handoff --tenant
  demo-tenant --team property-ops --sorx-url http://127.0.0.1:8787
  --operax-url http://127.0.0.1:8797 --webchat-url http://127.0.0.1:8080
  --bundle-dir /tmp/operax-pr24-no-start --force --no-start`
- generated WebChat provider pack contains
  `__greenticOperaxManagerSubmitHook`, `operax_manager_submit`, and
  `operax_manager_open`;
- generated default app pack contains `assets/cards/operax_dashboard.json`;
- `bash ci/local_check.sh`.

Follow-up fix:

- WebChat route output now uses the actual Greentic static route shape
  `/v1/web/webchat/demo/`.
- Injected WebChat cards now force absolute OperaX manager URLs such as
  `http://127.0.0.1:8797/v1/operax/manager/cards`, so browser-side card actions
  call OperaX instead of the WebChat server.
