# Import a Cloud backup in Kumiho Desktop

1. Download the `.kumiho` backup from Cloud **Settings → Workspace backup**.
2. In Desktop, select **Community Edition** and finish setup using a dedicated empty local Neo4j database. The installed CE server must support `import`; update it first when Desktop reports an unsupported version.
3. Open **Settings → General → Community Edition** and **Stop** the server. Leave Neo4j running and stop any other database writers.
4. Under **Import Cloud backup**, choose the file and click **Import and verify**. Keep Desktop open until the result appears.
5. After **Backup imported and verified**, click **Start** and inspect the restored projects and memories. Retain the Cloud data and original backup until this check is complete.

Desktop uses its saved CE configuration. It does not require copying credentials or typing commands. A nonempty database is refused without replacement or merging. External artifact files and Redis history are excluded from the graph archive.

The native command runs the server's offline archive check, atomic import with in-transaction verification, then an additional verification pass. CE start/stop operations share the import lock; running or unresolved CE processes and pending setup block import. Backup and config staging files are private on Unix, use the user's temporary directory on Windows, and are removed after the operation. Nothing is uploaded to Cloud. An operation timeout can leave an uncertain result: keep CE stopped, retain the backup, and verify before retrying.

## Validation

- `node scripts/test-ce-backup.cjs`
- `node scripts/test-ce-setup.cjs`
- `cargo test --locked --manifest-path src-tauri/Cargo.toml backup::tests`
- Opt-in native integration: set `KUMIHO_IMPORT_TEST_BINARY`, `KUMIHO_IMPORT_TEST_CONFIG` (dedicated disposable Neo4j only), and `KUMIHO_IMPORT_TEST_ARCHIVE` (synthetic web export), then run `cargo test --locked --manifest-path src-tauri/Cargo.toml backup::tests::real_ce_import -- --ignored --exact`. This imports into the configured database and verifies a repeat import is refused; never point it at customer data.

Requires both the Desktop UI change and a CE binary containing the import command. Source changes do not constitute a public release.

Local validation on 2026-09-26: Desktop executable build passed; three native backup unit tests and 11 existing CE lifecycle tests passed; the opt-in integration test rejected a corrupt archive, restored/verified a fresh web export in disposable Neo4j, and rejected repeat import. File-selection, CE setup, server-health and HTML5 file handling regressions passed. No public Desktop or CE release was published.
