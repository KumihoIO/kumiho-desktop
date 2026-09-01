const assert = require('node:assert/strict');
const fs = require('node:fs');

const ui = fs.readFileSync('desktop-ui/index.html', 'utf8');
const backend = fs.readFileSync('src-tauri/src/revka.rs', 'utf8');
const pty = fs.readFileSync('src-tauri/src/pty.rs', 'utf8');
const processTree = fs.readFileSync('src-tauri/src/process_tree.rs', 'utf8');
const revkaIcon = fs.readFileSync('desktop-ui/assets/revka-icon.png');

assert.deepEqual(
  revkaIcon.subarray(0, 8),
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  'the packaged Revka app icon must remain a PNG',
);
assert.match(
  ui,
  /<div class="app-icon"><img src="\.\/assets\/revka-icon\.png" alt=""><\/div><div class="grow"><div class="b">Revka<\/div>/,
);

assert.match(
  ui,
  /const result=await invoke\('revka_install'\);[\s\S]*const status=await invoke\('revka_status'\);[\s\S]*if\(!status\.onboarded\)\{[\s\S]*if\(openOnboardAfterInstall\) await openOnboardTerminal\(\);[\s\S]*\}else if\(\(status\.stale\|\|!status\.reachable\) && !await startRevka\(status\.reachable\)\)/,
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
assert.match(backend, /use std::net::\{[^}]*TcpListener/);
assert.match(backend, /fn address_is_bindable\([^)]*\)[\s\S]*TcpListener::bind/);
assert.match(backend, /if !port_is_bindable\(\)[\s\S]*port 42617 is occupied/);
assert.match(
  backend,
  /started_at\.elapsed\(\) < STARTUP_DEADLINE[\s\S]*Revka is already starting on 42617[\s\S]*if startup_timed_out[\s\S]*terminate_tracked/,
);
assert.match(backend, /struct TrackedRevka \{[\s\S]*process_tree: crate::process_tree::ProcessTree/);
assert.match(backend, /prepare_std_command\(&mut daemon\)[\s\S]*assign_std_child\(&child\)[\s\S]*write_runtime_stamp[\s\S]*resume_std_child\(&child\)/);
assert.match(processTree, /JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE/);
assert.match(processTree, /AssignProcessToJobObject/);
assert.match(processTree, /TerminateJobObject/);
assert.match(processTree, /signal_group\(group, libc::SIGTERM\)[\s\S]*wait_for_group\(group, TERM_GRACE\)/);
assert.match(processTree, /signal_group\(group, libc::SIGKILL\)[\s\S]*wait_for_group\(group, KILL_GRACE\)/);
assert.match(processTree, /impl Drop for ProcessTree[\s\S]*libc::SIGKILL/);
assert.match(pty, /\*guard = Some\(session\);[\s\S]*return Err\(message\)/);
assert.match(pty, /child\.wait\(\)[\s\S]*exited\.store\(true, Ordering::Release\)[\s\S]*let cleanup_error =[\s\S]*exit_process_tree\.terminate_after_leader_exit\(\)/);
assert.match(pty, /struct ExitCleanupState \{[\s\S]*running: bool,[\s\S]*error: Option<String>/);
assert.match(pty, /struct ExitCleanup \{[\s\S]*state: Mutex<ExitCleanupState>[\s\S]*completed: Condvar/);
assert.match(pty, /fn run\(&self,[\s\S]*state\.running = true[\s\S]*cleanup\(\)\.err\(\)[\s\S]*state\.running = false[\s\S]*notify_all/);
assert.match(pty, /fn wait_error\(&self\)[\s\S]*while state\.running[\s\S]*completed[\s\S]*wait\(state\)/);
assert.match(pty, /process_tree\.terminate\(\)\.err\(\)[\s\S]*exit_cleanup\.wait_error\(\)/);
assert.match(pty, /#\[cfg\(not\(windows\)\)\][\s\S]*fn fallback_kill\([^)]*\)[\s\S]*raw numeric PID[\s\S]*None/);
assert.match(pty, /prior_cleanup_error[\s\S]*could not fully stop the onboarding terminal/);
assert.match(processTree, /terminate_after_leader_exit[\s\S]*result\.is_err\(\)[\s\S]*pty_process_group\.store\(0/);
assert.match(processTree, /terminate_lock: std::sync::Mutex/);
assert.match(ui, /if\(message\.cleanup_error\)[\s\S]*TERM_CLEANUP_FAILED=true[\s\S]*expired process-group reference/);
assert.match(backend, /fn schedule_startup_watchdog\([\s\S]*sleep\(STARTUP_DEADLINE\)[\s\S]*same_start[\s\S]*terminate_tracked/);
assert.match(backend, /fn schedule_watchdog_for_new_start\([\s\S]*schedule_startup_watchdog/);
assert.match(backend, /pub fn revka_install\([\s\S]*schedule_watchdog_for_new_start\(&handle/);
assert.match(ui, /waitFor\(\(\)=>invoke\('revka_status'\)[\s\S]*95000\)/);
assert.match(
  backend,
  /matches!\(terminate_tracked\(&state\.revka, EXIT_GRACE\), Ok\(true\)\)/,
);

console.log('Revka lifecycle regression checks passed');
