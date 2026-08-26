const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {
  ceControlState,
  ceHealthReady,
  ceStartDisabled,
  neo4jPasswordError,
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

const html = fs.readFileSync(path.join(__dirname, '..', 'desktop-ui', 'index.html'), 'utf8');
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
assert.match(html, /password from this setup may not match the existing database/);
assert.match(html, /const pendingConfig=await invoke\('ce_configure_pending'\)[\s\S]*const ready=st\.reachable && await ceReady\(\)[\s\S]*if\(ready\)\{ await invoke\('ce_configure_commit'\)[\s\S]*if\(st\.reachable\) await stopCeAndWait\(\)[\s\S]*if\(pendingConfig\)[\s\S]*await invoke\('ce_configure_rollback'\);[\s\S]*await invoke\('ce_configure'/);
assert.match(html, /await invoke\('ce_configure_commit'\); configPending=false/);
assert.match(html, /if\(configPending\)[\s\S]*await invoke\('ce_configure_rollback'\)/);
assert.match(html, /safeToRollback=!serverStartAttempted[\s\S]*await stopCeAndWait\(\)[\s\S]*if\(safeToRollback\)/);
assert.match(html, /async function startCeAndWait\(\)\{[\s\S]*const current=await invoke\('ce_status'\);[\s\S]*if\(current\.reachable\)/);
assert.match(html, /async function ceReady\(\)[\s\S]*ceHealthReady\(await invoke\('ce_health'\)\)/);
assert.match(html, /const up=await waitFor\(ceReady, 40000\)/);
assert.match(html, /async function ceStop\(\)\{[\s\S]*beginCeAction\('stop'\)[\s\S]*await finishCeAction\(\)/);
assert.match(html, /async function stopCeAndWait\(\)\{[\s\S]*await invoke\('ce_stop'\)[\s\S]*!s\.reachable[\s\S]*It was not restarted/);
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

console.log('Community Edition setup regression checks passed');
