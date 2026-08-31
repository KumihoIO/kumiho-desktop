const assert = require('node:assert/strict');
const fs = require('node:fs');

const ui = fs.readFileSync('desktop-ui/index.html', 'utf8');
const backend = fs.readFileSync('src-tauri/src/revka.rs', 'utf8');
const pty = fs.readFileSync('src-tauri/src/pty.rs', 'utf8');
const commands = fs.readFileSync('src-tauri/src/main.rs', 'utf8');
const flow = require('../desktop-ui/revka-flow.js');

async function main() {
  assert.equal(flow.action({ installed: false }, false), 'install');
  assert.equal(flow.action({ installed: true, onboarded: false, stale: true }, false), 'update');
  assert.equal(flow.action({ installed: true, onboarded: false, stale: false }, false), 'onboard');
  assert.equal(flow.action({ installed: true, onboarded: true, reachable: false, stale: false }, false), 'start');
  assert.equal(flow.action({ installed: true, onboarded: true, reachable: true, stale: false }, false), 'dashboard');
  assert.equal(flow.ready({ onboarded: false, reachable: true, stale: false }), false);
  assert.equal(flow.ready({ onboarded: true, reachable: true, stale: false }), true);

  let releaseFirst;
  let calls = 0;
  const gate = flow.createRequestGate();
  const first = gate.run(() => {
    calls += 1;
    return new Promise((resolve) => { releaseFirst = resolve; });
  });
  const joined = gate.run(() => { calls += 1; });
  assert.strictEqual(joined, first);
  assert.equal(gate.pending(), true);
  await Promise.resolve();
  releaseFirst('ready');
  assert.equal(await first, 'ready');
  assert.equal(calls, 1);
  assert.equal(gate.pending(), false);
  await gate.run(() => { calls += 1; });
  assert.equal(calls, 2);

// Readiness is not installation: Revka creates a default config on daemon
// startup, while onboarding adds the operator workspace scaffold.
  assert.match(backend, /pub onboarded: bool/);
  assert.match(backend, /fn onboarding_artifacts_complete\(/);
  assert.match(backend, /\["AGENTS\.md", "USER\.md", "TOOLS\.md"\]/);
  assert.match(backend, /fn onboarding_complete\(\)[\s\S]*onboarding_artifacts_complete\(\)/);
  assert.match(backend, /fn start_revka\([\s\S]*if !onboarding_complete\(\)[\s\S]*run onboarding/i);

// Desktop owns the post-wizard daemon lifecycle. The CLI runs directly in the
// PTY so its exit is observable; an interactive shell would stay alive.
  assert.match(pty, /pub fn spawn_command_session\(/);
  assert.match(pty, /let exit_events = on_data\.clone\(\);[\s\S]*std::thread::spawn[\s\S]*child\.wait\(\)[\s\S]*PtyEvent::Exit/);
  assert.doesNotMatch(backend, /crate::pty::spawn_session\(&state, &binary/);
  assert.match(ui, /message\.type==='exit'[\s\S]*finishRevkaOnboarding/);
  assert.match(ui, /Start Revka now\? \(web dashboard \+ channels\)/);

// Pairing is issued natively over Revka's localhost-only admin contract before
// the dashboard iframe loads. Never parse decorated CLI output for the code.
  assert.match(backend, /127\.0\.0\.1:42617\/admin\/paircode/);
  assert.match(backend, /127\.0\.0\.1:42617\/admin\/paircode\/new/);
  assert.match(backend, /pub fn revka_pairing_prepare\(/);
  assert.match(backend, /pub fn revka_pairing_new\(/);
  assert.match(commands, /revka::revka_pairing_prepare/);
  assert.match(commands, /revka::revka_pairing_new/);
  assert.match(ui, /createRequestGate\(\)/);
  assert.match(ui, /onclick="issueRevkaPairingCode\(\)"/);
  assert.match(ui, /if\(replace\) invalidateRevkaPairingCode\(\);[\s\S]*invoke\(replace\?'revka_pairing_new'/);
  assert.match(ui, /function copyRevkaPairingCode\(\)[\s\S]*REVKA_PAIRING_REQUEST_GATE\.run/);
  assert.match(ui, /function openRevkaDashboardAfterPairing\(\)[\s\S]*REVKA_PAIRING_REQUEST_GATE\.run/);
  assert.match(ui, /closeRevkaPairing\(false\)[\s\S]*loadRevka\(\)/);
  assert.match(ui, /status\.stale\|\|!status\.reachable/);
  assert.match(ui, /TERM\.focus\(\)/);

// All product entry points gate on onboarding, not merely reachability.
  assert.match(ui, /const visiblyReachable=r\.onboarded&&r\.reachable&&!r\.stale/);
  assert.match(ui, /if\(!status\.onboarded\)[\s\S]*openOnboardTerminal/);
  assert.match(ui, /if\(!r\.onboarded\)[\s\S]*openOnboardTerminal/);

  console.log('Revka onboarding and pairing regression checks passed');
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
