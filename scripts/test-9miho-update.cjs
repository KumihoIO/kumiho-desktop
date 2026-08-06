const assert = require('node:assert/strict');
const fs = require('node:fs');

const pinned = JSON.parse(fs.readFileSync('src-tauri/9miho-version.json', 'utf8'));
const ui = fs.readFileSync('desktop-ui/index.html', 'utf8');

assert.deepEqual(pinned, {
  release_tag: '9miho-v0.3.0',
  version: '0.3.0',
  commit: '41ba633fe04738a820d0dab81e23b9d8b023d3f6',
});
assert.match(ui, /onclick="installOrUpdateMiho\(\)"/);
assert.match(ui, /button\.textContent=!miho\.installed\?'Install':\(miho\.update_available\?'Update':'Reinstall'\)/);
assert.match(
  ui,
  /await invoke\('miho_install'\);[\s\S]*if\(!await startMiho\(false\)\)[\s\S]*await invoke\('miho_status'\);[\s\S]*await refreshApps\(/,
);

console.log('9miho update regression checks passed');
