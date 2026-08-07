const assert = require('node:assert/strict');
const fs = require('node:fs');

const pinned = JSON.parse(fs.readFileSync('src-tauri/9miho-version.json', 'utf8'));
const ui = fs.readFileSync('desktop-ui/index.html', 'utf8');

assert.deepEqual(pinned, {
  release_tag: '9miho-v0.4.0',
  version: '0.4.0',
  commit: '7e5a82f26af12ac9b12e3687cd733f1cecbe8564',
});
assert.match(ui, /onclick="installOrUpdateMiho\(\)"/);
assert.match(ui, /invoke\('miho_check_update'\)/);
assert.match(ui, /if\(onlineUpdate\) await invoke\('miho_update'\); else await invoke\('miho_install'\)/);
assert.match(
  ui,
  /if\(!await startMiho\(false\)\)[\s\S]*await invoke\('miho_status'\);[\s\S]*await checkMihoUpdate\(false\);[\s\S]*await refreshApps\(/,
);

console.log('9miho update regression checks passed');
