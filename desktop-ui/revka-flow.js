(function(root,factory){
  const api=factory();
  if(typeof module==='object'&&module.exports) module.exports=api;
  else root.KumihoRevkaFlow=api;
})(typeof globalThis!=='undefined'?globalThis:this,function(){
  'use strict';

  function ready(status){
    return !!(status&&status.onboarded&&status.reachable&&!status.stale);
  }

  function action(status,updateAvailable){
    if(!status||!status.installed) return 'install';
    if(status.stale||updateAvailable) return 'update';
    if(!status.onboarded) return 'onboard';
    if(!status.reachable) return 'start';
    return 'dashboard';
  }

  // Coalesce UI requests that mutate the one-time pairing-code state. A
  // second click joins the first promise instead of rotating the code again.
  function createRequestGate(){
    let inFlight=null;
    return Object.freeze({
      run(task){
        if(inFlight) return inFlight;
        const tracked=Promise.resolve().then(task).finally(()=>{
          if(inFlight===tracked) inFlight=null;
        });
        inFlight=tracked;
        return tracked;
      },
      pending(){ return inFlight!==null; },
    });
  }

  return Object.freeze({ready,action,createRequestGate});
});
