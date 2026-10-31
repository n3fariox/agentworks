// The new-session dialog keeps secondary fields in collapsed sections. Specs that
// drive those fields start with every section open, as a returning user who had
// opened them would. A tab can pin its own state under `override`, because the
// order of several init scripts is not defined.
export const override='agentworks-test-create-sections';
export function openCreateSections(test){
  test.beforeEach(async({page})=>{
    // Init scripts also run in the app's iframes; only the page load may reset state.
    await page.addInitScript(key=>{if(window!==window.top)return;try{localStorage.setItem('agentworks-create-sections',sessionStorage.getItem(key)??JSON.stringify({'create-section-workspace':true,'create-section-model':true,'create-section-access':true}));}catch{}},override);
  });
}
