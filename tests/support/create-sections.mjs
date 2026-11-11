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

// Chooses a project and/or host through the combined place field, keeping the
// other half as it is. `project` may be an id or {label}.
export async function choosePlace(page,{project,host}={}){
  const [currentProject,currentHost]=await page.evaluate(()=>[document.getElementById('create-project').value,document.getElementById('create-host').value]);
  if(project&&typeof project==='object')project=await page.locator('#create-project option').filter({hasText:new RegExp(`^${project.label}$`)}).getAttribute('value');
  await page.locator('#create-place').click();
  await page.locator(`#create-place-list .create-place-option[data-project="${project??currentProject}"][data-host="${host??currentHost}"]`).click();
}
