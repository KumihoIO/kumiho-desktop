const assert = require('node:assert/strict');
const { fileError, importBackup } = require('../desktop-ui/backup.js');
(async () => {
  assert.match(fileError(null), /Choose/);
  assert.match(fileError({ name: 'backup.cypher', size: 100 }), /cypher-shell/);
  assert.match(fileError({ name: 'error.html', size: 5 }), /\.kumiho/);
  assert.match(fileError({ name: 'backup.kumiho', size: 0 }), /nonempty/);
  assert.match(fileError({ name: 'backup.kumiho', size: 64 * 1024 * 1024 + 1 }), /64 MiB/);
  const bytes = new TextEncoder().encode('{"file":"백업"}');
  const file = { name: 'cloud.kumiho', size: bytes.length, arrayBuffer: async () => bytes.buffer };
  const calls = [], statuses = [];
  assert.equal(await importBackup({ file, status: s => statuses.push(s), invoke: async (...args) => { calls.push(args); return 'verified'; } }), 'verified');
  assert.deepEqual(calls, [['ce_import_backup', { archive: '{"file":"백업"}' }]]);
  assert.equal(statuses.length, 2);
  await assert.rejects(importBackup({ file: { ...file, arrayBuffer: async () => new Uint8Array([255]).buffer }, status: () => {}, invoke: () => { throw Error('must not invoke'); } }), /encoded data/);
  await assert.rejects(importBackup({ file, status: () => {}, invoke: async () => { throw Error('database contains data'); } }), /database contains data/);
  console.log('PASS: backup selection, size limits, exact UTF-8 handoff and backend failure propagation');
})();
