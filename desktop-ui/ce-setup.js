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

  function ceSetupFailureMessage(error, logTail, databaseResult, readyTimedOut) {
    const rawError = String(error || '');
    const tail = String(logTail || '');
    const database = String(databaseResult || '');
    const authFailure = /unauthorized|authentication|invalid credentials|wrong password/i.test(rawError + '\n' + tail);
    const reusedDatabase = /^Neo4j (?:already serving|container .* started)/.test(database);
    let message;
    if (authFailure || reusedDatabase) {
      message = 'The server could not connect to Neo4j. The password from this setup may not match the existing database; use its original password or reset only a fresh, disposable setup.';
    } else if (readyTimedOut) {
      message = 'Server did not become ready within 40s — check Docker/Neo4j, then use Start again.';
    } else {
      message = rawError || 'Community Edition could not start.';
    }
    const details = [];
    if (rawError && !readyTimedOut) details.push(rawError);
    if (tail && !rawError.includes(tail)) details.push(tail);
    if (details.length) message += '\n— kumiho_server log (~/.kumiho/logs/kumiho_server.log) —\n' + details.join('\n');
    return message;
  }

  async function completeCeSetupStart(options) {
    const { invoke, waitForReady, stopCeAndWait, databaseResult } = options;
    let readyTimedOut = false;
    try {
      // Treat startup as attempted before awaiting the command. If the bridge
      // rejects after spawning, cleanup must still prove the process is down
      // before restoring the previous config.
      await invoke('ce_start');
      const ready = await waitForReady();
      if (!ready) {
        readyTimedOut = true;
        throw new Error('Community Edition readiness timed out');
      }
      await invoke('ce_configure_commit');
      return { ok: true, configPending: false, cleanupBlocked: false, message: '' };
    } catch (error) {
      let tail = '';
      try { tail = await invoke('ce_log_tail'); } catch (_) {}
      let message = ceSetupFailureMessage(error, tail, databaseResult, readyTimedOut);
      try {
        const stopped = await stopCeAndWait(true);
        if (stopped !== true) throw new Error('Community Edition process exit was not confirmed');
      } catch (stopError) {
        message += '\nSetup failed, but Community Edition could not be stopped. The pending config was preserved: ' + String(stopError);
        return { ok: false, configPending: true, cleanupBlocked: true, message };
      }
      try {
        await invoke('ce_configure_rollback');
        return { ok: false, configPending: false, cleanupBlocked: false, message };
      } catch (rollbackError) {
        message += '\nSetup failed and the previous config could not be restored: ' + String(rollbackError);
        return { ok: false, configPending: true, cleanupBlocked: false, message };
      }
    }
  }

  async function rollbackPendingCeSetup(options) {
    const { invoke, stopCeAndWait } = options;
    const stopped = await stopCeAndWait(true);
    if (stopped !== true) {
      throw new Error('Community Edition process exit was not confirmed; the pending config was preserved');
    }
    await invoke('ce_configure_rollback');
  }

  const api = {
    ceControlState,
    ceHealthReady,
    ceSetupFailureMessage,
    ceStartDisabled,
    completeCeSetupStart,
    neo4jPasswordError,
    rollbackPendingCeSetup,
  };
  root.KumihoDesktopCeSetup = api;
  if (typeof module !== 'undefined' && module.exports) {
    module.exports = api;
  }
})(typeof globalThis !== 'undefined' ? globalThis : this);
