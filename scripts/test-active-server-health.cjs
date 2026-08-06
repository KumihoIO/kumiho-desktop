const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {
  activeServerReachable,
  healthRequestIsCurrent,
  serverHealthTitle,
} = require('../desktop-ui/server-health.js');

assert.equal(activeServerReachable('ce', true, false), true);
assert.equal(activeServerReachable('ce', false, true), false);
assert.equal(activeServerReachable('cloud', false, true), true);
assert.equal(activeServerReachable('cloud', true, false), false);
assert.equal(serverHealthTitle('ce', true), 'Community Edition connected');
assert.equal(serverHealthTitle('ce', false), 'Community Edition stopped');
assert.equal(serverHealthTitle('ce', false, 'check-failed'), 'Community Edition health check failed');
assert.equal(serverHealthTitle('cloud', true), 'Kumiho Cloud connected');
assert.equal(serverHealthTitle('cloud', false), 'Kumiho Cloud unreachable');
assert.equal(serverHealthTitle('cloud', false, 'check-failed'), 'Kumiho Cloud health check failed');
assert.equal(serverHealthTitle('cloud', false, 'missing-token'), 'Kumiho Cloud token is not configured');
assert.equal(serverHealthTitle('cloud', false, 'rejected-token'), 'Kumiho Cloud rejected the saved token');
assert.equal(healthRequestIsCurrent('ce', 1, 'ce', 1), true);
assert.equal(healthRequestIsCurrent('ce', 1, 'cloud', 1), false);
assert.equal(healthRequestIsCurrent('cloud', 1, 'cloud', 2), false);

const html = fs.readFileSync(path.join(__dirname, '..', 'desktop-ui', 'index.html'), 'utf8');
assert.match(html, /<script src="\.\/server-health\.js"><\/script>/);
assert.match(html, /id="dot-server"/);
assert.match(html, /KumihoDesktopStatus\.activeServerReachable\(mode,/);
assert.match(html, /mode === 'cloud'[\s\S]*cloudProbe\(token\)/);
assert.match(html, /const mode=MODE;[\s\S]*const generation=SERVER_HEALTH_GENERATION;/);
assert.match(html, /healthRequestIsCurrent\(health\.mode,health\.generation,MODE,SERVER_HEALTH_GENERATION\)/);
const invalidateHealth = html.match(/function invalidateServerHealth\(\)\{([\s\S]*?)\n  \}/);
assert.ok(invalidateHealth, 'invalidateServerHealth should remain present');
assert.doesNotMatch(invalidateHealth[1], /STATUS_REFRESH_IN_FLIGHT=null/);
assert.match(html, /const refreshGeneration=SERVER_HEALTH_GENERATION;/);
assert.match(html, /if\(refreshIsCurrent\(\)\)[\s\S]*brain_status/);
assert.match(html, /refreshApps\(mihoStatus,refreshGeneration\)/);
assert.match(html, /async function refreshApps\(mihoStatus=null,expectedGeneration=null\)/);
assert.match(html, /if\(STATUS_REFRESH_PENDING\)\{ STATUS_REFRESH_PENDING=false; queueMicrotask\(\(\)=>refreshStatus\(\)\); \}/);
assert.match(html, /catch\(e\)\{[\s\S]*serverHealthTitle\(serverMode,false,'check-failed'\)/);
const refreshRun = html.match(/async function refreshRun\(\)\{([\s\S]*?)\n  \}/);
assert.ok(refreshRun, 'refreshRun should remain present');
assert.doesNotMatch(refreshRun[1], /\$\('dot-server'\)/);

console.log('active server health regression checks passed');
