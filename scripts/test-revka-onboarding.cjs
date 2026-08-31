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

// Desktop owns the post-wizard daemon lifecycle. The PTY uses PowerShell on
// Windows and the user's default shell on macOS/Linux, then makes that shell
// exit with the Revka wizard's status so completion remains observable.
  assert.match(pty, /pub fn spawn_command_session\(/);
  assert.match(pty, /REVKA_INTERACTIVE/);
  assert.match(pty, /fn platform_shell\(/);
  assert.match(pty, /pwsh(?:\.exe)?/i);
  assert.match(pty, /powershell(?:\.exe)?/i);
  assert.match(pty, /new_default_prog\(\)\.get_shell\(\)/);
  assert.match(pty, /cmd\.env\("KUMIHO_REVKA_BIN", revka\.as_os_str\(\)\)/);
  assert.match(pty, /exec \\"\$KUMIHO_REVKA_BIN\\" onboard/);
  assert.match(pty, /exec \$env\.KUMIHO_REVKA_BIN onboard/);
  assert.match(pty, /PathBuf::from\("\/bin\/sh"\)/);
  assert.doesNotMatch(pty, /"revka onboard"/);
  assert.match(pty, /args: vec!\["-c"\.into\(\), command\.into\(\)\]/);
  assert.match(backend, /revka_pty_start\.lock\(\)/);
  assert.match(pty, /let exit_events = on_data\.clone\(\);[\s\S]*std::thread::spawn[\s\S]*child\.wait\(\)[\s\S]*PtyEvent::Exit/);
  assert.doesNotMatch(backend, /crate::pty::spawn_session\(&state, &binary/);
  assert.match(ui, /new T\.core\.Channel\(\)/);
  assert.doesNotMatch(ui, /T\.ipc\.Channel/);
  assert.match(ui, /function openOnboardTerminal\(\)\{[\s\S]*if\(REVKA_ONBOARD_OPENING\) return REVKA_ONBOARD_OPENING;[\s\S]*openOnboardTerminalOnce\(\)/);
  assert.match(ui, /async function installOrUpdateRevka\(openOnboardAfterInstall=true\)/);
  assert.match(ui, /if\(!status\.onboarded\)\{\s*if\(openOnboardAfterInstall\) await openOnboardTerminal\(\);\s*\}else if/);
  assert.match(ui, /if\(!status\.installed\)\{[\s\S]*installOrUpdateRevka\(false\);[\s\S]*status=await invoke\('revka_status'\)/);
  assert.match(ui, /if\(PTY_OPEN \|\| TERM_CLEANUP_FAILED \|\| REVKA_ONBOARD_FINISHING\)/);
  assert.match(ui, /PTY_OPEN=true; PTY_READY=false;/);
  assert.match(ui, /function sendPtyInput\([\s\S]*if\(!PTY_READY\)/);
  assert.match(ui, /if\(myGen!==TERM_GEN \|\| !PTY_OPEN \|\| !PTY_READY \|\| TERM_CLOSING\) return;/);
  assert.match(ui, /Could not send input to Revka/);
  assert.match(ui, /Could not resize the Revka terminal/);
  assert.match(ui, /Could not answer Revka\\'s final start prompt automatically/);
  assert.match(ui, /message\.type==='exit'[\s\S]*message\.cleanup_error[\s\S]*finishRevkaOnboarding/);
  assert.match(ui, /Start Revka now\? \(web dashboard \+ channels\)/);

  const closeStart = ui.indexOf('async function closeOnboardTerminal()');
  const closeEnd = ui.indexOf('// Keep keyboard focus', closeStart);
  const closeFlow = ui.slice(closeStart, closeEnd);
  assert.ok(closeStart >= 0 && closeEnd > closeStart, 'close onboarding flow must exist');
  assert.ok(
    closeFlow.indexOf("await invoke('revka_pty_stop')") < closeFlow.indexOf("$('onboard-term').classList.remove('show')"),
    'the modal must remain visible until PTY stop succeeds',
  );
  assert.match(closeFlow, /catch\(error\)\{[\s\S]*Could not stop the Revka onboarding terminal/);
  assert.doesNotMatch(closeFlow, /revka_pty_stop'\);\s*\}catch\([^)]*\)\{\}/);

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
