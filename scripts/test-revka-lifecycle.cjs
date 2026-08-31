const assert = require('node:assert/strict');
const fs = require('node:fs');

const ui = fs.readFileSync('desktop-ui/index.html', 'utf8');
const backend = fs.readFileSync('src-tauri/src/revka.rs', 'utf8');

assert.match(
  ui,
  /const result=await invoke\('revka_install'\);[\s\S]*const status=await invoke\('revka_status'\);[\s\S]*if\(!status\.onboarded\) await openOnboardTerminal\(\);[\s\S]*else if\(\(status\.stale\|\|!status\.reachable\) && !await startRevka\(status\.reachable\)\)/,
);
assert.match(ui, /const visiblyReachable=r\.onboarded&&r\.reachable&&!r\.stale;/);
assert.match(
  ui,
  /const revkaReady=KumihoRevkaFlow\.ready\(revka\);[\s\S]*pill\(\$\('revka-app-pill'\),revkaReady/,
);
assert.match(
  ui,
  /if\(force\)\{[\s\S]*await invoke\('revka_stop'\);[\s\S]*catch\(e\)\{ toast\(String\(e\),true\); return false; \}/,
);

assert.doesNotMatch(backend, /taskkill"\)\s*\.args\(\["\/IM", binary_name\(\)/);
assert.doesNotMatch(backend, /command\("taskkill"\)/);
assert.doesNotMatch(backend, /command\("pkill"\)/);
assert.doesNotMatch(backend, /fn terminate_recorded_revka/);
assert.match(backend, /struct RuntimeStamp \{[\s\S]*identity: String,/);
assert.match(backend, /libc::kill\(child\.id\(\) as libc::pid_t, libc::SIGTERM\)/);
assert.match(backend, /Desktop will not signal a cross-session PID/);
assert.match(backend, /runtime_state\(\)[\s\S]*validated_runtime_stamp\(\)/);
assert.match(
  backend,
  /Some\(Ok\(None\)\) if !reachable[\s\S]*Revka is already starting on 42617[\s\S]*if reachable \{/,
);
assert.match(
  backend,
  /matches!\(terminate_tracked\(&state\.revka, EXIT_GRACE\), Ok\(true\)\)/,
);

console.log('Revka lifecycle regression checks passed');
