// Real Desktop native import against an owned disposable Neo4j only.
const { execFile, spawn } = require('node:child_process');
const { promisify } = require('node:util');
const { mkdtemp, writeFile, rm } = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const exec = promisify(execFile);
(async () => {
  const [binary, archive] = process.argv.slice(2);
  if (!binary || !archive) throw Error('Usage: node scripts/test-ce-backup-integration.cjs ABS_CE_BINARY ABS_SYNTHETIC_ARCHIVE');
  const temp = await mkdtemp(path.join(os.tmpdir(), 'kumiho-desktop-backup-test-'));
  const container = 'kumiho-desktop-backup-test-' + process.pid;
  const password = 'desktop-import-test-only';
  let created = false;
  const docker = (...args) => exec('docker', args, { timeout: 90000, windowsHide: true });
  try {
    await docker('run', '-d', '--name', container, '-p', '127.0.0.1::7687', '-e', 'NEO4J_AUTH=neo4j/' + password, '-e', 'NEO4J_server_memory_heap_initial__size=128m', '-e', 'NEO4J_server_memory_heap_max__size=256m', '-e', 'NEO4J_server_memory_pagecache_size=128m', 'neo4j:5');
    created = true;
    const address = (await docker('port', container, '7687/tcp')).stdout.trim();
    if (!/^127\.0\.0\.1:\d+$/.test(address)) throw Error('Unexpected test endpoint');
    let ready = false;
    for (let i = 0; i < 40; i++) {
      try { await docker('exec', container, 'cypher-shell', '-u', 'neo4j', '-p', password, 'RETURN 1'); ready = true; break; }
      catch { await new Promise(resolve => setTimeout(resolve, 1000)); }
    }
    if (!ready) throw Error('Disposable Neo4j did not start');
    const config = path.join(temp, 'CE test settings.toml');
    await writeFile(config, ['deployment_mode = "self_hosted_ce"', 'eula_accepted = true', 'eula_version = "1.1"', 'db_uri = ""', 'db_name = "neo4j"', 'db_user = "neo4j"', 'db_pass = "' + password + '"', 'neo4j_port = ' + address.split(':')[1]].join('\n'), { mode: 0o600 });
    const env = { ...process.env, KUMIHO_IMPORT_TEST_BINARY: path.resolve(binary), KUMIHO_IMPORT_TEST_ARCHIVE: path.resolve(archive), KUMIHO_IMPORT_TEST_CONFIG: config };
    await new Promise((resolve, reject) => {
      const child = spawn('cargo', ['test', '--locked', '--manifest-path', 'src-tauri/Cargo.toml', 'backup::tests::real_ce_import', '--', '--ignored', '--exact'], { env, windowsHide: true, stdio: 'inherit' });
      child.on('error', reject);
      child.on('exit', code => code === 0 ? resolve() : reject(Error('Native import test failed: ' + code)));
    });
    console.log('PASS: Desktop native command rejects a corrupt archive, imports and verifies the web backup, and refuses a duplicate import');
  } finally {
    if (created) await docker('rm', '-f', '-v', container);
    const resolved = path.resolve(temp);
    if (path.dirname(resolved) !== path.resolve(os.tmpdir()) || !path.basename(resolved).startsWith('kumiho-desktop-backup-test-')) throw Error('Refusing unexpected cleanup path');
    await rm(resolved, { recursive: true, force: true });
  }
})().catch(error => { console.error(error.message); process.exitCode = 1; });
