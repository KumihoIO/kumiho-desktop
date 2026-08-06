(function exposeServerHealth(root) {
  function activeServerReachable(mode, ceReachable, cloudReachable) {
    return mode === 'cloud' ? cloudReachable === true : ceReachable === true;
  }

  function healthRequestIsCurrent(requestMode, requestGeneration, currentMode, currentGeneration) {
    return requestMode === currentMode && requestGeneration === currentGeneration;
  }

  function serverHealthTitle(mode, reachable, reason) {
    if (mode === 'cloud') {
      if (reachable) return 'Kumiho Cloud connected';
      if (reason === 'check-failed') return 'Kumiho Cloud health check failed';
      if (reason === 'missing-token') return 'Kumiho Cloud token is not configured';
      if (reason === 'rejected-token') return 'Kumiho Cloud rejected the saved token';
      return 'Kumiho Cloud unreachable';
    }
    if (!reachable && reason === 'check-failed') return 'Community Edition health check failed';
    return reachable ? 'Community Edition connected' : 'Community Edition stopped';
  }

  root.KumihoDesktopStatus = { activeServerReachable, healthRequestIsCurrent, serverHealthTitle };
  if (typeof module !== 'undefined' && module.exports) {
    module.exports = { activeServerReachable, healthRequestIsCurrent, serverHealthTitle };
  }
})(typeof globalThis !== 'undefined' ? globalThis : this);
