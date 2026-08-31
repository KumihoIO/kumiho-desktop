const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {
  ceControlState,
  ceHealthReady,
  ceSetupFailureMessage,
  ceStartDisabled,
  completeCeSetupStart,
  neo4jPasswordError,
  rollbackPendingCeSetup,
  startCeAutoboot,
  startCeRuntime,
} = require('../desktop-ui/ce-setup.js');

assert.equal(neo4jPasswordError(''), 'Set a Neo4j password.');
assert.match(neo4jPasswordError('1234567'), /at least 8 characters/i);
assert.equal(neo4jPasswordError('12345678'), '');
assert.equal(neo4jPasswordError('여덟글자암호임요'), '');

assert.equal(ceStartDisabled(false, false), false);
assert.equal(ceStartDisabled(true, false), true);
assert.equal(ceStartDisabled(false, true), true);
assert.deepEqual(ceControlState(false, false, false, true), {
  startDisabled: false,
  restartDisabled: true,
  stopDisabled: true,
});
assert.deepEqual(ceControlState(true, false, false, true), {
  startDisabled: true,
  restartDisabled: false,
  stopDisabled: false,
});
assert.deepEqual(ceControlState(true, true, false, true), {
  startDisabled: true,
  restartDisabled: true,
  stopDisabled: true,
});
assert.deepEqual(ceControlState(false, false, true, false), {
  startDisabled: true,
  restartDisabled: true,
  stopDisabled: false,
});
assert.deepEqual(ceControlState(true, false, true, false), {
  startDisabled: true,
  restartDisabled: true,
  stopDisabled: false,
});
assert.equal(ceHealthReady({ status: 'ok', neo4j: { status: 'ok' } }), true);
assert.equal(ceHealthReady({ status: 'degraded', neo4j: { status: 'error' } }), false);
assert.equal(ceHealthReady({ status: 'ok' }), false);
assert.match(ceSetupFailureMessage('authentication failure', '', '', false), /password from this setup may not match/i);
assert.match(ceSetupFailureMessage('', '', 'Neo4j already serving 7687 — reusing', true), /existing database/i);
assert.match(ceSetupFailureMessage('', '', '', true), /did not become ready within 40s/i);

const html = fs.readFileSync(path.join(__dirname, '..', 'desktop-ui', 'index.html'), 'utf8');
const databasesCard = html.indexOf('<span class="b">Databases</span>');
const vectorSearchCard = html.indexOf('<span class="b">Vector search</span>');
const startupCard = html.indexOf('<span class="b">Startup</span>');
assert.ok(
  databasesCard >= 0 && databasesCard < vectorSearchCard && vectorSearchCard < startupCard,
  'General settings should place Vector search between Databases and Startup',
);
const ceSetupSource = fs.readFileSync(path.join(__dirname, '..', 'desktop-ui', 'ce-setup.js'), 'utf8');
const dockerSource = fs.readFileSync(path.join(__dirname, '..', 'src-tauri', 'src', 'docker.rs'), 'utf8');
const mainSource = fs.readFileSync(path.join(__dirname, '..', 'src-tauri', 'src', 'main.rs'), 'utf8');
const runSource = fs.readFileSync(path.join(__dirname, '..', 'src-tauri', 'src', 'run.rs'), 'utf8');
const releaseWorkflowSource = fs.readFileSync(path.join(__dirname, '..', '.github', 'workflows', 'desktop-release.yml'), 'utf8');
const releaseCiSource = fs.readFileSync(path.join(__dirname, '..', '.github', 'workflows', 'desktop-ci.yml'), 'utf8');
const releaseGateSource = fs.readFileSync(path.join(__dirname, 'verify-desktop-release.cjs'), 'utf8');
const releaseGateTestSource = fs.readFileSync(path.join(__dirname, 'test-desktop-release-gate.cjs'), 'utf8');
assert.match(html, /<script src="\.\/ce-setup\.js"><\/script>/);
assert.match(html, /id="f-pass"[^>]*minlength="8"[^>]*oninput="validateNeo4jPassword\(\)"/);
assert.match(html, /id="f-pass"[^>]*aria-invalid="false"/);
assert.match(html, /id="f-pass-help"[^>]*aria-live="polite"/);
assert.match(html, /setAttribute\('aria-invalid',error\?'true':'false'\)/);
assert.match(html, /KumihoDesktopCeSetup\.neo4jPasswordError\(pass\)/);
assert.match(html, /id="ce-start-btn"[^>]*onclick="ceStart\(\)"/);
assert.match(html, /id="ce-restart-btn"[^>]*onclick="ceRestart\(\)"/);
assert.match(html, /id="ce-stop-btn"[^>]*onclick="ceStop\(\)"/);
assert.match(html, /ceControlState\(CE_LAST_REACHABLE,CE_STARTING,CE_LAST_MANAGED,CE_LAST_STOPPABLE\)/);
assert.match(html, /CE_STARTING=true;[\s\S]*\$\('ce-btn'\)\.disabled=true/);
assert.doesNotMatch(html, /if\(!st\.configured\)/);
assert.match(ceSetupSource, /password from this setup may not match the existing database/);
assert.match(html, /const pendingConfig=await invoke\('ce_configure_pending'\)[\s\S]*const ready=st\.reachable && await ceReady\(\)[\s\S]*if\(ready\)\{ await invoke\('ce_configure_commit'\)[\s\S]*if\(pendingConfig\)[\s\S]*rollbackPendingCeSetup\(\{invoke,stopCeAndWait\}\)[\s\S]*else if\(st\.reachable\|\|st\.managed\) await stopCeAndWait\(\)[\s\S]*await invoke\('ce_configure'/);
assert.match(html, /completeCeSetupStart\(\{[\s\S]*invoke, databaseResult, stopCeAndWait,[\s\S]*waitForReady:\(\)=>waitFor\(ceReady,40000\)/);
assert.match(html, /configPending=outcome\.configPending; cleanupBlocked=outcome\.cleanupBlocked/);
assert.match(html, /if\(configPending&&!cleanupBlocked\)[\s\S]*await invoke\('ce_configure_rollback'\)/);
assert.match(ceSetupSource, /await invoke\('ce_start'\)[\s\S]*await invoke\('ce_configure_commit'\)/);
assert.match(ceSetupSource, /catch \(error\)[\s\S]*await stopCeAndWait\(true\)[\s\S]*await invoke\('ce_configure_rollback'\)/);
assert.match(html, /async function startCeAndWait\(\)\{[\s\S]*const current=await invoke\('ce_status'\);[\s\S]*if\(current\.reachable\)/);
assert.match(html, /startCeRuntime\(\{[\s\S]*invoke, stopCeAndWait, waitForReady:\(\)=>waitFor\(ceReady,40000\)/);
assert.match(html, /async function ceReady\(\)[\s\S]*ceHealthReady\(await invoke\('ce_health'\)\)/);
assert.match(html, /async function ceStop\(\)\{[\s\S]*beginCeAction\('stop'\)[\s\S]*await finishCeAction\(\)/);
assert.match(html, /async function stopCeAndWait\(force=false\)\{[\s\S]*!current\.reachable&&!current\.managed&&!force[\s\S]*await invoke\('ce_stop',\{force\}\)[\s\S]*!s\.reachable[\s\S]*It was not restarted/);
assert.match(html, /async function ceRestart\(\)\{[\s\S]*beginCeAction\('restart'\)[\s\S]*await stopCeAndWait\(\)[\s\S]*await startCeAndWait\(\)/);
assert.match(html, /manualRecovery=!CE_LAST_STOPPABLE&&\(CE_LAST_REACHABLE\|\|CE_LAST_MANAGED\)[\s\S]*Manual stop \/ recovery steps[\s\S]*update\.disabled=CE_STARTING\|\|manualRecovery/);
assert.match(html, /st\.reachable&&st\.stoppable===false[\s\S]*Desktop will not signal that process/);
assert.match(html, /st\.managed&&st\.stoppable===false[\s\S]*Recover \/ manual stop steps/);
assert.match(html, /async function dbUp\(\)\{ if\(!beginCeAction\('db-start'\)\)return;[\s\S]*finally\{ await finishCeAction\(false\); \} \}/);
assert.match(html, /async function dbDown\(\)\{ if\(!beginCeAction\('db-stop'\)\)return;[\s\S]*invoke\('docker_down'\)[\s\S]*invoke\('docker_status'\)[\s\S]*still appears to be running[\s\S]*finally\{ await finishCeAction\(false\); \} \}/);
assert.doesNotMatch(html, /setTimeout\(\(\)=>cmd\('ce_start'/);
assert.match(html, /async function ceUpdate\(\)\{[\s\S]*beginCeAction\('update'\)[\s\S]*await finishCeAction\(\)/);
assert.match(html, /const ready=ce\.reachable && await ceReady\(\)[\s\S]*if\(!ready && beginCeAction\('boot'\)\)[\s\S]*catch\(e\)\{ const message='Community Edition could not start automatically:[\s\S]*setLog\('general',message,true\)[\s\S]*finally \{ await finishCeAction\(false\); \}/);
assert.doesNotMatch(html, /withTimeout\(invoke\('docker_up'/);
assert.match(html, /startCeAutoboot\(\{[\s\S]*startDatabases:async\(\)=>[\s\S]*await invoke\('docker_up'[\s\S]*startServer:startCeAndWait/);
assert.match(html, /invoke\('docker_up',[\s\S]{0,180}timeoutMs:30000/);
assert.match(html, /Community Edition did not become ready — open Settings → General to inspect its current status and retry/);
assert.doesNotMatch(html, /Community Edition did not become ready and was stopped/);
assert.doesNotMatch(html, /Community Edition is still starting/);
assert.match(ceSetupSource, /previous config is still pending cleanup/);
assert.match(html, /failureMessage=outcome\.message[\s\S]*if\(configPending&&!cleanupBlocked\)[\s\S]*Previous config was restored during cleanup[\s\S]*failureMessage\?failureMessage/);
assert.match(html, /\$\('settings'\)\.classList\.contains\('show'\)[\s\S]*\$\('settings-mode'\)\.value==='ce'[\s\S]*renderCeRuntimeStatus\(ce\)/);
assert.match(html, /id="ce-log"[^>]*role="status"[^>]*aria-live="polite"/);
assert.match(html, /id="log-general"[^>]*role="status"[^>]*aria-live="polite"/);
assert.doesNotMatch(dockerSource, /\["rm",\s*"-f"/, 'managed database containers must never be deleted automatically');
assert.match(dockerSource, /fn docker_command_output[\s\S]*child\.try_wait\(\)[\s\S]*child\.kill\(\)[\s\S]*ErrorKind::TimedOut/);
assert.match(dockerSource, /DOCKER_CAPTURE_LIMIT[\s\S]*struct DockerCapture[\s\S]*fn read_bounded[\s\S]*pub async fn docker_status[\s\S]*spawn_blocking[\s\S]*pub async fn docker_up[\s\S]*spawn_blocking/);
assert.doesNotMatch(dockerSource, /fn spawn_output_reader|fn receive_output/);
assert.match(dockerSource, /timeout_ms:\s*Option<u64>[\s\S]*DockerDeadline::after/);
assert.match(dockerSource, /fn inspect_found[\s\S]*no such object[\s\S]*Docker inspect failed[\s\S]*fn docker_down_blocking[\s\S]*find_container_checked[\s\S]*could not stop/);
assert.match(runSource, /fn neo4j_password_error[\s\S]*NEO4J_MIN_PASSWORD_LENGTH[\s\S]*pub fn ce_configure[\s\S]*neo4j_password_error\(&neo4j_password\)/);
assert.match(runSource, /struct CeProcessMarker[\s\S]*fn process_identity[\s\S]*fn active_ce_process_marker/);
assert.match(runSource, /CE_PROCESS_INTENT_FILE[\s\S]*write_private_file\(&intent_path, b"starting"\)[\s\S]*sync_directory\(&home\)[\s\S]*cmd\.spawn\(\)[\s\S]*write_ce_process_marker/);
assert.match(runSource, /fn move_file_durable[\s\S]*MoveFileExW[\s\S]*fn write_private_file_atomic[\s\S]*write_ce_process_marker/);
assert.doesNotMatch(runSource, /fn stop_recorded_ce|libc::kill\(marker\.pid/);
assert.match(runSource, /pub fn ce_stop[\s\S]*state\.ce_start\.lock[\s\S]*stop_tracked_child[\s\S]*active_ce_process_marker/);
assert.doesNotMatch(runSource, /taskkill[\s\S]{0,100}"\/IM",\s*"kumiho_server\.exe"|pkill[\s\S]{0,100}kumiho_server/);
assert.match(runSource, /pub struct CeStatus[\s\S]*pub managed: bool[\s\S]*pub stoppable: bool[\s\S]*CE_PROCESS_MARKER_FILE[\s\S]*CE_PROCESS_INTENT_FILE/);
assert.match(runSource, /fn sync_directory[\s\S]*directory\.sync_all[\s\S]*fn publish_private_file[\s\S]*move_file_durable\(source, destination, true\)/);
assert.match(runSource, /pub fn kill_pending_ce[\s\S]*state\.ce_start\.lock[\s\S]*setup_config_pending_at[\s\S]*stop_tracked_child/);
assert.match(mainSource, /RunEvent::ExitRequested[\s\S]*run::kill_pending_ce\(app\)/);
assert.match(releaseWorkflowSource, /Verify release tag matches Desktop version[\s\S]*LOCAL_TAG_COMMIT[\s\S]*node scripts\/verify-desktop-release\.cjs/);
assert.match(releaseWorkflowSource, /EXPECTED_SHA: \$\{\{ github\.sha \}\}[\s\S]*getRef\(\{ owner, repo, ref: `tags\/\$\{tag\}` \}\)[\s\S]*ensureDraftRelease/);
assert.doesNotMatch(releaseWorkflowSource, /if:\s*github\.ref_type\s*==\s*'tag'/);
assert.match(releaseGateSource, /localTagCommit !== expectedSha[\s\S]*resolveRemoteTagCommit[\s\S]*target_commitish:[\s\S]*assertRemoteTagCommit/);
assert.match(releaseGateTestSource, /refType: 'branch'[\s\S]*localTagCommit: MOVED_SHA[\s\S]*postChecks/);
assert.match(releaseCiSource, /pull_request:[\s\S]*windows-latest[\s\S]*ubuntu-22\.04[\s\S]*macos-15[\s\S]*macos-15-intel[\s\S]*cargo test --manifest-path src-tauri\/Cargo\.toml --locked/);

const inlineScripts = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)];
assert.ok(inlineScripts.length > 0, 'the inline application script should remain present');
for (const [, source] of inlineScripts) new Function(source);

async function testSetupTransactions() {
  {
    const calls = [];
    let finishDatabases;
    const databases = new Promise((resolve) => { finishDatabases = resolve; });
    const pending = startCeAutoboot({
      startDatabases: async () => { calls.push('databases-start'); await databases; calls.push('databases-done'); },
      startServer: async () => { calls.push('server-start'); return { up: true }; },
    });
    await Promise.resolve();
    assert.deepEqual(calls, ['databases-start']);
    finishDatabases();
    assert.deepEqual(await pending, { up: true });
    assert.deepEqual(calls, ['databases-start', 'databases-done', 'server-start']);
  }

  {
    const calls = [];
    await assert.rejects(
      startCeAutoboot({
        startDatabases: async () => { calls.push('databases'); throw new Error('Docker is still warming up'); },
        startServer: async () => { calls.push('server'); return { up: false }; },
      }),
      /Docker is still warming up/,
    );
    assert.deepEqual(calls, ['databases']);
  }

  {
    const calls = [];
    let stopCalls = 0;
    const outcome = await completeCeSetupStart({
      databaseResult: 'Neo4j already serving 7687 — reusing',
      invoke: async (command) => {
        calls.push(command);
        if (command === 'ce_start') throw new Error('authentication failure: unauthorized');
        if (command === 'ce_log_tail') return '[bolt] client is unauthorized';
        if (command === 'ce_configure_rollback') return 'restored';
        throw new Error('unexpected command: ' + command);
      },
      waitForReady: async () => { throw new Error('readiness must not run after a start rejection'); },
      stopCeAndWait: async () => { stopCalls += 1; return true; },
    });
    assert.equal(outcome.ok, false);
    assert.equal(outcome.configPending, false);
    assert.equal(outcome.cleanupBlocked, false);
    assert.match(outcome.message, /password from this setup may not match the existing database/i);
    assert.deepEqual(calls, ['ce_start', 'ce_log_tail', 'ce_configure_rollback']);
    assert.equal(stopCalls, 1);
  }

  {
    const calls = [];
    const outcome = await completeCeSetupStart({
      databaseResult: 'Neo4j already serving 7687 — reusing',
      invoke: async (command) => {
        calls.push(command);
        if (command === 'ce_start') throw new Error('authentication failure');
        if (command === 'ce_log_tail') return '[bolt] client is unauthorized';
        if (command === 'ce_configure_rollback') throw new Error('temporary disk error');
        throw new Error('unexpected command: ' + command);
      },
      waitForReady: async () => false,
      stopCeAndWait: async () => true,
    });
    assert.equal(outcome.ok, false);
    assert.equal(outcome.configPending, true);
    assert.equal(outcome.cleanupBlocked, false);
    assert.match(outcome.message, /password from this setup may not match/i);
    assert.match(outcome.message, /still pending cleanup: .*temporary disk error/i);
    assert.deepEqual(calls, ['ce_start', 'ce_log_tail', 'ce_configure_rollback']);
  }

  {
    const calls = [];
    let stopCalls = 0;
    const outcome = await completeCeSetupStart({
      databaseResult: 'Neo4j container created',
      invoke: async (command) => {
        calls.push(command);
        if (command === 'ce_start') return 'starting';
        if (command === 'ce_log_tail') return '';
        if (command === 'ce_configure_rollback') return 'restored';
        throw new Error('unexpected command: ' + command);
      },
      waitForReady: async () => false,
      stopCeAndWait: async () => { stopCalls += 1; return true; },
    });
    assert.equal(outcome.ok, false);
    assert.equal(outcome.configPending, false);
    assert.match(outcome.message, /did not become ready within 40s/i);
    assert.deepEqual(calls, ['ce_start', 'ce_log_tail', 'ce_configure_rollback']);
    assert.equal(stopCalls, 1);
  }

  {
    const calls = [];
    let stopCalls = 0;
    const outcome = await completeCeSetupStart({
      databaseResult: 'Neo4j container created',
      invoke: async (command) => {
        calls.push(command);
        if (command === 'ce_start') return 'serving';
        if (command === 'ce_configure_commit') return 'committed';
        throw new Error('unexpected command: ' + command);
      },
      waitForReady: async () => true,
      stopCeAndWait: async () => { stopCalls += 1; },
    });
    assert.deepEqual(outcome, { ok: true, configPending: false, cleanupBlocked: false, message: '' });
    assert.deepEqual(calls, ['ce_start', 'ce_configure_commit']);
    assert.equal(stopCalls, 0);
  }

  {
    const calls = [];
    const outcome = await completeCeSetupStart({
      databaseResult: 'Neo4j already serving 7687 — reusing',
      invoke: async (command) => {
        calls.push(command);
        if (command === 'ce_start') throw new Error('authentication failure');
        if (command === 'ce_log_tail') return '';
        if (command === 'ce_configure_rollback') throw new Error('rollback must not run');
        throw new Error('unexpected command: ' + command);
      },
      waitForReady: async () => false,
      stopCeAndWait: async () => false,
    });
    assert.equal(outcome.ok, false);
    assert.equal(outcome.configPending, true);
    assert.equal(outcome.cleanupBlocked, true);
    assert.match(outcome.message, /process exit was not confirmed/i);
    assert.match(outcome.message, /pending config was preserved/i);
    assert.deepEqual(calls, ['ce_start', 'ce_log_tail']);
  }

  {
    const calls = [];
    let forced = false;
    await rollbackPendingCeSetup({
      invoke: async (command) => { calls.push(command); return 'restored'; },
      // Models a retry where ce_status.reachable is false but the backend still
      // owns an unbound child from the previous attempt.
      stopCeAndWait: async (force) => { forced = force; return true; },
    });
    assert.equal(forced, true);
    assert.deepEqual(calls, ['ce_configure_rollback']);
  }

  {
    const calls = [];
    await assert.rejects(
      rollbackPendingCeSetup({
        invoke: async (command) => { calls.push(command); },
        stopCeAndWait: async () => false,
      }),
      /process exit was not confirmed/i,
    );
    assert.deepEqual(calls, []);
  }

  {
    const calls = [];
    let stopCalls = 0;
    const outcome = await startCeRuntime({
      invoke: async (command) => { calls.push(command); return 'starting'; },
      waitForReady: async () => false,
      stopCeAndWait: async (force) => { assert.equal(force, true); stopCalls += 1; return true; },
    });
    assert.deepEqual(outcome, { up: false, result: 'starting' });
    assert.deepEqual(calls, ['ce_start']);
    assert.equal(stopCalls, 1);
  }

  {
    let stopCalls = 0;
    await assert.rejects(
      startCeRuntime({
        invoke: async () => { throw new Error('spawn failed'); },
        waitForReady: async () => true,
        stopCeAndWait: async () => { stopCalls += 1; return true; },
      }),
      /spawn failed/i,
    );
    assert.equal(stopCalls, 1);
  }
}

testSetupTransactions()
  .then(() => console.log('Community Edition setup regression checks passed'))
  .catch((error) => { console.error(error); process.exitCode = 1; });
