const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const configPath = path.join(__dirname, '..', 'src-tauri', 'tauri.conf.json');
const config = JSON.parse(fs.readFileSync(configPath, 'utf8'));
const mainWindow = config.app?.windows?.find((window) => window.label === 'main');

assert.ok(mainWindow, 'the main Tauri window must be configured');
assert.equal(
  mainWindow.dragDropEnabled,
  false,
  'native Tauri file drop must be disabled so embedded 9miho receives HTML5 drag/drop events',
);

console.log('HTML5 file drop configuration check passed');
