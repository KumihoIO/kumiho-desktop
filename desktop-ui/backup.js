(function (root) {
  'use strict';
  const MAX_BYTES = 64 * 1024 * 1024;
  function fileError(file) {
    if (!file) return 'Choose a .kumiho backup file.';
    if (/\.cypher$/i.test(file.name)) return 'Cypher scripts run with cypher-shell. For Desktop import, download the recommended .kumiho format from your Cloud dashboard.';
    if (!/\.kumiho$/i.test(file.name)) return 'Choose a .kumiho backup downloaded from your Cloud dashboard.';
    if (!file.size || file.size > MAX_BYTES) return 'Choose a nonempty backup no larger than 64 MiB.';
    return '';
  }
  async function importBackup({ file, invoke, status }) {
    const error = fileError(file);
    if (error) throw new Error(error);
    status('Reading backup…');
    // Fatal decoding preserves the exported bytes: never silently replace
    // malformed UTF-8 in an archive protected by a checksum.
    const archive = new TextDecoder('utf-8', { fatal: true }).decode(await file.arrayBuffer());
    status('Validating, importing and verifying… Keep Desktop open. This may take a few minutes.');
    return invoke('ce_import_backup', { archive });
  }
  const api = { fileError, importBackup };
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.KumihoDesktopBackup = api;
})(typeof globalThis !== 'undefined' ? globalThis : this);
