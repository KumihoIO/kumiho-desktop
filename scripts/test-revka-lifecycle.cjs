const assert = require('node:assert/strict');
const fs = require('node:fs');

const ui = fs.readFileSync('desktop-ui/index.html', 'utf8');
const backend = fs.readFileSync('src-tauri/src/revka.rs', 'utf8');

assert.match(
  ui,
  /if\(!await startRevka\(false\)\) throw new Error\('Revka was installed but did not restart'\);[\s\S]*setLog\('apps','✓ '\+result\);/,
);
assert.match(ui, /const visiblyReachable=r\.reachable&&!r\.stale;/);
assert.match(
  ui,
  /pill\(\$\('revka-app-pill'\),revka\.reachable&&!revka\.stale,revka\.stale\?'restart required'/,
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
