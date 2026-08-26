(function exposeCeSetup(root) {
  const NEO4J_MIN_PASSWORD_LENGTH = 8;

  function neo4jPasswordError(value) {
    const length = Array.from(String(value || '').trim()).length;
    if (length === 0) return 'Set a Neo4j password.';
    if (length < NEO4J_MIN_PASSWORD_LENGTH) {
      return 'Use at least ' + NEO4J_MIN_PASSWORD_LENGTH + ' characters for the Neo4j password (' + length + '/' + NEO4J_MIN_PASSWORD_LENGTH + ').';
    }
    return '';
  }

  function ceStartDisabled(reachable, starting) {
    return reachable === true || starting === true;
  }

  function ceControlState(reachable, busy) {
    const running = reachable === true;
    const working = busy === true;
    return {
      startDisabled: ceStartDisabled(running, working),
      restartDisabled: !running || working,
      stopDisabled: !running || working,
    };
  }

  function ceHealthReady(health) {
    return !!health && health.status === 'ok' && !!health.neo4j && health.neo4j.status === 'ok';
  }

  root.KumihoDesktopCeSetup = { ceControlState, ceHealthReady, ceStartDisabled, neo4jPasswordError };
  if (typeof module !== 'undefined' && module.exports) {
    module.exports = { ceControlState, ceHealthReady, ceStartDisabled, neo4jPasswordError };
  }
})(typeof globalThis !== 'undefined' ? globalThis : this);
