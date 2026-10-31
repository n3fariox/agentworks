// The new-session dialog keeps secondary fields in collapsible sections. Each
// summary names the current choice, so a closed section still says what applies.
// Open/closed state is remembered per browser; sections start closed.
export function setupCreateSections({$,storage}) {
  const key='agentworks-create-sections';
  let saved={};try{saved=JSON.parse(storage.getItem(key))||{};}catch{}
  const sections=[...document.querySelectorAll('#create-form .create-section')];
  for(const section of sections){
    section.open=!!saved[section.id];
    // Only the person's own toggles are remembered, not sections opened for them.
    // The click runs before the summary flips the section, so it saves the new state.
    section.querySelector('summary').addEventListener('click',()=>{
      saved[section.id]=!section.open;try{storage.setItem(key,JSON.stringify(saved));}catch{}
    });
  }
  const chosen=id=>$(id).selectedOptions[0]?.textContent.trim()||'';
  function summarize(){
    const mode=$('workspace-mode').value;
    const workspace=[chosen('workspace-mode')];
    if(mode==='newWorktree'&&$('worktree-branch').value.trim())workspace.push($('worktree-branch').value.trim());
    if(mode==='existingWorktree')workspace.push($('existing-worktree').value?chosen('existing-worktree').split(' · ')[0]:'none selected');
    $('create-section-workspace-value').textContent=workspace.join(' · ');
    const model=[chosen('create-model')];
    if(!$('create-model-provider-field').hidden&&$('create-model-provider').value)model.unshift(chosen('create-model-provider'));
    if($('create-role').value)model.push(chosen('create-role'));
    $('create-section-model-value').textContent=model.join(' · ');
    const access=[$('create-agent-access').value?chosen('create-agent-access').split(' · ')[0]:chosen('create-agent-access').replace(/^Use /,'')];
    if($('create-resume-policy').checked)access.push('resumes after quota reset');
    $('create-section-access-value').textContent=access.join(' · ');
    // An existing-worktree mode cannot submit until one is picked, so show the picker.
    const picker=$('create-section-workspace');
    if(mode==='existingWorktree'&&!$('existing-worktree').value&&!$('workspace-mode').disabled)picker.open=true;
  }
  $('create-form').addEventListener('change',summarize);
  $('create-form').addEventListener('input',summarize);
  return {summarize};
}
