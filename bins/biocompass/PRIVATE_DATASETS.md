# Private dataset drafts and publication

`biocompass-dataset` is the local Rust supervisor for dataset edits and first publication. It authorizes scientists with the existing BioCompass hub and runs the pinned importer and correction renderer in network-isolated OCI containers. Each dataset has its own editors and publishers. The service binds to loopback. Run it under an operator account with a private storage directory and access to the container engine. Give scientists access through the authenticated API. The workspace files and container engine socket are trusted operator resources.

## Adopt an existing import

Build the binary with `cargo build -p biocompass --bin biocompass-dataset`. Set these environment variables in the operator's shell:

| Variable | Value |
|---|---|
| `BC_DATASET_STORE` | A dedicated, private directory outside the harness's legacy catalog discovery directories. |
| `BC_DRAFT_WORKSPACE` | The workspace previously passed to `bc-hla-import validate`. |
| `BC_IDENTITY_HUB` | The issuing BioCompass hub origin. HTTPS or loopback HTTP is supported. The store records its issuer at provisioning. |
| `BC_VALIDATOR_IMAGE` | An immutable local image ID for the original validation worker. |
| `BC_OVERLAY_IMAGE` | An immutable local image ID for a compatible correction renderer, such as `bc-hla-import` 0.2.0. |
| `BC_CONTAINER_ENGINE` | Path to the Docker-compatible container engine, default `docker`. |

The operator adopts the existing draft by its content ID. The supervisor verifies the retained archive and reruns the original worker. Its report must match the saved step-1 report.

```sh
biocompass-dataset provision --dataset study --org "$ORG_ID" --draft "$DRAFT_ID" \
  --editor "$SCIENTIST_USER_ID" --publisher "$CURATOR_USER_ID"
biocompass-dataset serve --port 18961
```

Each subsequent request uses an existing scientist's `Authorization: Bearer` session token. No credentials are stored with the audit. An unavailable identity hub prevents authenticated actions until the hub returns.

## Edit, publish and verify

| Operation | Request | Outcome |
|---|---|---|
| Inspect | `GET /v1/datasets/study` | An authorized editor or publisher sees the current draft revision, audit and private release, if any. |
| Edit | `POST /v1/datasets/study/edit` | Supply `expected_revision`, `patch` and an optional `reason`. The patch contains the canonical table, full natural row key, and matching `before` and `after` field names. One row must match. The supervisor revalidates and records the actor, old and new values, UTC time, reason and new revision. |
| Publish | `POST /v1/datasets/study/publish` | Supply `{"expected_revision":1}` for revision 1. A publisher can publish a valid revision once. The response contains the private release and its manifest. |
| Verify | `GET /v1/datasets/study/verify` | Reopens the DuckLake and checks the stored manifests, snapshot and object bytes against the private release. |

If a request loses its reply, retry the same body with the same user's session. Concurrent edits use revision checks. A publish retry returns the committed version. A partial construction remains invisible until the atomic head update. The published version includes the original upload, the validated bundle, a sealed DuckLake snapshot, exact worker identities and its audit.

The private storage and `bc-private-v1/` catalog namespace separate supervised releases from marketplace discovery and legacy table-level imports.

## Run Target Validation on a private version

Set `BC_TARGET_VALIDATION_MODULE` to the approved module directory and start `biocompass-dataset serve`. The module manifest must declare `biocompass.target-validation.v1`, reference an immutable OCI digest, and disable networking. The supervisor authorizes the current user through the identity hub and uses the existing native Quote and Confirm flow. The runner receives four read-only Parquet projections from the pinned snapshot.

| Operation | Request |
|---|---|
| Prepare | `POST /v1/datasets/study/runs/prepare` with `{"version":1,"search_type":"protein","search_term":"Q99999"}`. Use `"peptide"` for peptide search and add `hla_filter` to filter eligible samples. |
| Confirm | `POST /v1/datasets/study/runs/<run_id>/confirm` with `{"idempotency_key":"unique-for-this-run"}`. The response includes the results and verified receipt. |
| Download | `GET /v1/datasets/study/runs/<run_id>/artifacts/raw_data.csv`. Use the same scientist’s authenticated session. |

Missing required capabilities return HTTP 422 with the missing input names before the dispatcher can issue a quote. Zero matching observations are a valid result when the input tables are present. Private runs remain scoped to the authorizing account and dataset version.

## Publish from a personal laptop

Start `biocompass up` on a loopback address. Open its **Local dataset owner** link in the browser on that laptop. Background startup writes the link in the private `owner-link.txt` file in the data directory. The link is valid for ten minutes and can be redeemed once. The unlocked tab may operate for eight hours. After closing the tab, open the current link from that same private file. Redeeming a new link ends the previous owner session. An expired link refreshes the file without stopping the workbench. The Froglet private keys remain on the laptop.

Use the installed import module to upload the original file. To change an existing private dataset, open its history in Data Explorer, select an action offered by its installed importer, preview the effects, then publish. Each publication has its own catalog ID, original data pin and signed receipt. After a restart the previous versions reopen at their original IDs. A signed-in cloud or organization account does not establish local ownership.

The first owner operation on an older local release uses the stored Froglet requester key that originally published it to bind its original digest to the laptop's Froglet node key. The state file records this signed delegation and the historical release bytes remain intact. Keep the entire private data directory, including both original key files, when moving the installation.

Headless local clients can read the protected launch file and redeem its fragment by posting JSON with a `ticket` field to `/api/v1/local/owner-session` on the exact loopback origin in the link. Send the returned `owner_session` in `X-BioCompass-Local-Owner` with local importer and release-change requests. The process accepts one active owner session at a time. Treat that session and the launch file like credentials.

## Prepare and publish a dataset change

Set `BC_VERSION_WORKER_IMAGE` to the immutable local image ID of the importer image containing `bc_hla_import.version_changes`. The supervisor also uses the dataset's original pinned validation image.

To stage an append, run `bc-hla-import validate study-append.zip --workspace "$BC_DATASET_STORE/study/append-sources"` as the operator and record its draft ID. The appended bundle may rely on references present in the published dataset. The supervisor validates the merged result. Keep the append staging directory private and scoped to that dataset.

An editor sends a JSON request to `POST /v1/datasets/study/versions/preview` using their BioCompass session. Example requests are below.

| Change | Request body |
|---|---|
| Append | `{"base_version":1,"kind":"append","draft_id":"sha256-<digest>"}` |
| Remove a sample | `{"base_version":2,"kind":"remove_sample","sample_id":"S1"}` |
| Correct a row | `{"base_version":2,"kind":"correct_row","patch":{...}}` |
| Restore the data from an older version | `{"base_version":3,"kind":"revert","restore_version":1}` |

The preview returns a change ID and its effects. A removal reports counts per linked table and up to 20 row keys per table. A publisher confirms through `POST /v1/datasets/study/versions/<change_id>/publish`. On an intervening release, publication rechecks the change against the current head. A changed removal preview or conflicting append returns HTTP 409. Retrying an already published change returns its original version.

The dataset response exposes the current release and its history. Use the required `version` field when preparing Target Validation. The saved quote pins that version through confirmation, including after a restart.

## Hosted release changes in the workbench

An assigned editor can preview the changes declared by a reviewed importer. When an action calls for a validated upload or prior release, the editor chooses an eligible source for this dataset. The publisher reviews the source identifier, its validated SHA-256, the field values and the affected resources before confirming the stored preview.

The browser supports exact integer inputs and counts from −9,007,199,254,740,991 through 9,007,199,254,740,991. If a receipt contains a larger integer in magnitude, the drawer marks that value as outside the supported range, shows the receipt identifiers, and makes that receipt read-only.

## Grant scoped access to queries and runs

The local provider accepts its primary identity hub and the optional `BC_PARTNER_IDENTITY_HUB` origin configured by the operator. For a partner session, send `X-BioCompass-Issuer` with that exact origin. The provider verifies each session at its specified issuer. Dataset editing and publication use the primary issuer.

A dataset publisher can send a JSON request to `POST /v1/datasets/study/query-grants` to set a grant for an issuer and organization. Set `access` to `rows`, `compute_only` or `revoked`. The `samples` array lists sample IDs, with an empty array for revocation. For example, `{"issuer":"http://127.0.0.1:18200","org_id":"org-b","access":"compute_only","samples":["S3"]}` admits Target Validation for that sample. Use a rows grant to allow named reads. Changing the access level, narrowing the scope or revoking the grant invalidates its existing quotes. Expanding the scope applies to new quotes. An existing run keeps its original sample set.

For an HLA dataset, set `"sample_classes":["cell line"]` to grant access to every matching sample in a chosen release. Omit `samples` when using class selection. Class names match the published values exactly. An unclassified sample cannot match. The provider evaluates classes against the sealed roster pinned to the request. A new release can include newly classified samples while a run pinned to an earlier release keeps its original data. Switching between ID and class selection invalidates earlier quotes, even if the two selections currently contain the same samples.

Send `POST /v1/datasets/study/query` with a version and named query. The direct query names are `sample_roster.v1`, `sample_roster.v2` and `cohort_observation_counts.v1`. All accept an optional `sample_class` string. The provider binds parameters and filters sample relations before joins or aggregation. Each request rolls back its temporary tables and views on the shared DuckDB connection.

For roster pagination, select `sample_roster.v2` and set `page_size` to 1–1000 (default 1000). If the response includes `next_after_sample_id`, send it as `after_sample_id` with the same version, query and selectors. The cursor is exclusive, in lexicographic sample-ID order. Its absence marks the last page. Each request checks the current grant.

Modules can declare `sample_roster.v2` or `sample_observation_counts.v2` in their manifest. These use the same pagination parameters inside each live query's `params` and return the cursor alongside `rows`. A count page includes one row per authorized sample and zero for samples without observations. Each page uses the original run snapshot and grant, and counts toward the run's query and byte limits. Existing v1 handles retain their row limit and `sample_observation_counts.v1` requires an individual `sample_id`.

Active native confirmations monitor the current grant and terminate the container if its pinned scope is revoked. Subsequent queries and artifact downloads check the current policy. Result files already downloaded are held by the recipient.
