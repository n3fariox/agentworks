import {localState as localStorage} from './hosting.js';
// Files and Diff fill the conversation area while keeping the chat mounted.
// On phones the panel is a history entry so Back returns to the chat.
// Each tab is a {show(session),hide()} renderer; the panel only follows the
// active session and decides which tab is visible.
const KEY='agentworks-side-panel';
export const TABS=['files','diff'];

export function setupSidePanel({panel,tabs,triggers={},close,renderers,getActive,phone}){
  let saved={};try{saved=JSON.parse(localStorage.getItem(KEY))||{};}catch{}
  const state={open:false,tab:TABS.includes(saved.tab)?saved.tab:'files'};
  // Only the desktop open state is remembered; phones always start on the chat.
  const save=()=>localStorage.setItem(KEY,JSON.stringify({open:state.open&&!phone(),tab:state.tab}));
  function render(){
    panel.hidden=!state.open;
    panel.dataset.tab=state.tab;
    for(const [name,button] of Object.entries(tabs)){
      const selected=state.open&&name===state.tab;
      button.setAttribute('aria-selected',String(selected));button.tabIndex=selected?0:-1;
      document.getElementById(button.getAttribute('aria-controls')).hidden=name!==state.tab;
    }
    for(const [name,button] of Object.entries(triggers))button.setAttribute('aria-expanded',String(state.open&&name===state.tab));
    for(const name of TABS)if(!state.open||name!==state.tab)renderers[name].hide?.();
    const session=getActive();if(state.open&&session)renderers[state.tab].show(session);
  }
  function open(tab=state.tab,{history:push=true}={}){
    if(!getActive())return;
    const wasOpen=state.open;state.open=true;state.tab=TABS.includes(tab)?tab:state.tab;save();render();
    if(push&&!wasOpen&&phone()&&history.state?.agentworks!=='panel'){
      // Back from the panel always lands on the chat it was opened from.
      if(history.state?.agentworks!=='chat')history.replaceState({agentworks:'chat'},'');
      history.pushState({agentworks:'panel'},'');
    }
  }
  function hide({history:pop=true}={}){
    if(!state.open)return;
    // On phones the open panel is its own history entry: leave it with Back.
    if(pop&&history.state?.agentworks==='panel'){history.back();return;}
    state.open=false;save();render();
  }
  for(const [name,button] of Object.entries(tabs))button.addEventListener('click',()=>open(name));
  for(const [name,button] of Object.entries(triggers))button.addEventListener('click',()=>{if(state.open&&state.tab===name)hide();else open(name);});
  // Arrow keys move between tabs, as in a WAI-ARIA tablist.
  tabs[TABS[0]].parentElement.addEventListener('keydown',event=>{
    if(event.key!=='ArrowLeft'&&event.key!=='ArrowRight')return;
    const next=TABS[(TABS.indexOf(state.tab)+(event.key==='ArrowRight'?1:TABS.length-1))%TABS.length];
    open(next);tabs[next].focus();event.preventDefault();
  });
  close.addEventListener('click',()=>hide());
  panel.addEventListener('keydown',event=>{if(event.key==='Escape'&&!document.querySelector('dialog[open]')){hide();event.preventDefault();}});
  return {
    open,close:hide,
    get isOpen(){return state.open;},
    get tab(){return state.tab;},
    // A newly selected session refreshes the visible tab. Desktop restores a
    // panel left open; phones keep the chat in front.
    sessionChanged(){if(phone()){if(state.open)hide({history:false});return;}if(!state.open&&saved.open){saved.open=false;state.open=true;}render();},
    // Back/Forward on phones.
    popstate(entry){if(entry==='panel'){if(!state.open)open(state.tab,{history:false});}else if(state.open)hide({history:false});},
  };
}
