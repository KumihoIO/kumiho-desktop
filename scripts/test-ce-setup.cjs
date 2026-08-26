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
} = require('../desktop-ui/ce-setup.js');

assert.equal(neo4jPasswordError(''), 'Set a Neo4j password.');
assert.match(neo4jPasswordError('1234567'), /at least 8 characters/i);
assert.equal(neo4jPasswordError('12345678'), '');
assert.equal(neo4jPasswordError('여덟글자암호임요'), '');

assert.equal(ceStartDisabled(false, false), false);
assert.equal(ceStartDisabled(true, false), true);
assert.equal(ceStartDisabled(false, true), true);
assert.deepEqual(ceControlState(false, false), {
  startDisabled: false,
  restartDisabled: true,
  stopDisabled: true,
});
assert.deepEqual(ceControlState(true, false), {
  startDisabled: true,
  restartDisabled: false,
  stopDisabled: false,
});
assert.deepEqual(ceControlState(true, true), {
  startDisabled: true,
  restartDisabled: true,
  stopDisabled: true,
});
assert.equal(ceHealthReady({ status: 'ok', neo4j: { status: 'ok' } }), true);
assert.equal(ceHealthReady({ status: 'degraded', neo4j: { status: 'error' } }), false);
assert.equal(ceHealthReady({ status: 'ok' }), false);
assert.match(ceSetupFailureMessage('authentication failure', '', '', false), /password from this setup may not match/i);
assert.match(ceSetupFailureMessage('', '', 'Neo4j already serving 7687 — reusing', true), /existing database/i);
assert.match(ceSetupFailureMessage('', '', '', true), /did not become ready within 40s/i);

const html = fs.readFileSync(path.join(__dirname, '..', 'desktop-ui', 'index.html'), 'utf8');
const ceSetupSource = fs.readFileSync(path.join(__dirname, '..', 'desktop-ui', 'ce-setup.js'), 'utf8');
const dockerSource = fs.readFileSync(path.join(__dirname, '..', 'src-tauri', 'src', 'docker.rs'), 'utf8');
assert.match(html, /<script src="\.\/ce-setup\.js"><\/script>/);
assert.match(html, /id="f-pass"[^>]*minlength="8"[^>]*oninput="validateNeo4jPassword\(\)"/);
assert.match(html, /id="f-pass"[^>]*aria-invalid="false"/);
assert.match(html, /id="f-pass-help"[^>]*aria-live="polite"/);
assert.match(html, /setAttribute\('aria-invalid',error\?'true':'false'\)/);
assert.match(html, /KumihoDesktopCeSetup\.neo4jPasswordError\(pass\)/);
assert.match(html, /id="ce-start-btn"[^>]*onclick="ceStart\(\)"/);
assert.match(html, /id="ce-restart-btn"[^>]*onclick="ceRestart\(\)"/);
assert.match(html, /id="ce-stop-btn"[^>]*onclick="ceStop\(\)"/);
assert.match(html, /ceControlState\(CE_LAST_REACHABLE,CE_STARTING\)/);
assert.match(html, /CE_STARTING=true;[\s\S]*\$\('ce-btn'\)\.disabled=true/);
assert.doesNotMatch(html, /if\(!st\.configured\)/);
assert.match(ceSetupSource, /password from this setup may not match the existing database/);
assert.match(html, /const pendingConfig=await invoke\('ce_configure_pending'\)[\s\S]*const ready=st\.reachable && await ceReady\(\)[\s\S]*if\(ready\)\{ await invoke\('ce_configure_commit'\)[\s\S]*if\(pendingConfig\)[\s\S]*rollbackPendingCeSetup\(\{invoke,stopCeAndWait\}\)[\s\S]*else if\(st\.reachable\) await stopCeAndWait\(\)[\s\S]*await invoke\('ce_configure'/);
assert.match(html, /completeCeSetupStart\(\{[\s\S]*invoke, databaseResult, stopCeAndWait,[\s\S]*waitForReady:\(\)=>waitFor\(ceReady,40000\)/);
assert.match(html, /configPending=outcome\.configPending; cleanupBlocked=outcome\.cleanupBlocked/);
assert.match(html, /if\(configPending&&!cleanupBlocked\)[\s\S]*await invoke\('ce_configure_rollback'\)/);
assert.match(ceSetupSource, /await invoke\('ce_start'\)[\s\S]*await invoke\('ce_configure_commit'\)/);
assert.match(ceSetupSource, /catch \(error\)[\s\S]*await stopCeAndWait\(true\)[\s\S]*await invoke\('ce_configure_rollback'\)/);
assert.match(html, /async function startCeAndWait\(\)\{[\s\S]*const current=await invoke\('ce_status'\);[\s\S]*if\(current\.reachable\)/);
assert.match(html, /async function ceReady\(\)[\s\S]*ceHealthReady\(await invoke\('ce_health'\)\)/);
assert.match(html, /async function ceStop\(\)\{[\s\S]*beginCeAction\('stop'\)[\s\S]*await finishCeAction\(\)/);
assert.match(html, /async function stopCeAndWait\(force=false\)\{[\s\S]*!current\.reachable&&!force[\s\S]*await invoke\('ce_stop'\)[\s\S]*!s\.reachable[\s\S]*It was not restarted/);
assert.match(html, /async function ceRestart\(\)\{[\s\S]*beginCeAction\('restart'\)[\s\S]*await stopCeAndWait\(\)[\s\S]*await startCeAndWait\(\)/);
assert.doesNotMatch(html, /setTimeout\(\(\)=>cmd\('ce_start'/);
assert.match(html, /async function ceUpdate\(\)\{[\s\S]*beginCeAction\('update'\)[\s\S]*await finishCeAction\(\)/);
assert.match(html, /const ready=ce\.reachable && await ceReady\(\)[\s\S]*if\(!ready && beginCeAction\('boot'\)\)[\s\S]*catch\(e\)\{ toast\('Community Edition could not start automatically:[\s\S]*finally \{ await finishCeAction\(false\); \}/);
assert.match(html, /\$\('settings'\)\.classList\.contains\('show'\)[\s\S]*\$\('settings-mode'\)\.value==='ce'[\s\S]*renderCeRuntimeStatus\(ce\)/);
assert.match(html, /id="ce-log"[^>]*role="status"[^>]*aria-live="polite"/);
assert.match(html, /id="log-general"[^>]*role="status"[^>]*aria-live="polite"/);
assert.doesNotMatch(dockerSource, /\["rm",\s*"-f"/, 'managed database containers must never be deleted automatically');

const inlineScripts = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)];
assert.ok(inlineScripts.length > 0, 'the inline application script should remain present');
for (const [, source] of inlineScripts) new Function(source);

async function testSetupTransactions() {
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
}

testSetupTransactions()
  .then(() => console.log('Community Edition setup regression checks passed'))
  .catch((error) => { console.error(error); process.exitCode = 1; });
