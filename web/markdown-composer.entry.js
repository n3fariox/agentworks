import {EditorState,StateField,Compartment,Transaction,Prec} from '@codemirror/state';
import {EditorView,Decoration,WidgetType,keymap,placeholder} from '@codemirror/view';
import {history,historyKeymap,defaultKeymap,insertNewlineAndIndent} from '@codemirror/commands';
import {markdown,insertNewlineContinueMarkup} from '@codemirror/lang-markdown';
import {syntaxTree} from '@codemirror/language';

class Bullet extends WidgetType{
  toDOM(){const node=document.createElement('span');node.className='cm-md-bullet';node.textContent='•';return node;}
  eq(){return true;}
  ignoreEvent(){return false;}
}
// These decorations only change the view. The document, clipboard, drafts and
// submitted prompt always contain the original Markdown, including its spacing.
function previewDecorations(state){
  const ranges=[],selection=state.selection.main;
  const editing=(from,to)=>selection.empty?selection.head>from&&selection.head<to:selection.from<to&&selection.to>from;
  const mark=(from,to,cls)=>{if(to>from)ranges.push(Decoration.mark({class:cls}).range(from,to));};
  const hide=(from,to)=>{if(to>from)ranges.push(Decoration.replace({}).range(from,to));};
  const line=(pos,cls)=>ranges.push(Decoration.line({class:cls}).range(state.doc.lineAt(pos).from));
  syntaxTree(state).iterate({enter(node){
    const {name,from,to}=node;
    if(/^ATXHeading[1-6]$/.test(name)){
      line(from,'cm-md-heading cm-md-h'+name.at(-1));
      const header=node.node.firstChild;
      if(header&&selection.head>header.to)hide(header.from,Math.min(to,header.to+1));
    }else if(name==='StrongEmphasis'||name==='Emphasis'||name==='Strikethrough'||name==='InlineCode'){
      mark(from,to,{StrongEmphasis:'cm-md-strong',Emphasis:'cm-md-em',Strikethrough:'cm-md-strike',InlineCode:'cm-md-inline-code'}[name]);
      if(!editing(from,to))for(let child=node.node.firstChild;child;child=child.nextSibling){
        if(/^(EmphasisMark|StrikethroughMark|CodeMark)$/.test(child.name))hide(child.from,child.to);
      }
    }else if(name==='Link'||name==='Image'){
      mark(from,to,'cm-md-link');
      if(!editing(from,to))for(let child=node.node.firstChild;child;child=child.nextSibling){
        if(/^(LinkMark|URL|LinkTitle)$/.test(child.name))hide(child.from,child.to);
      }
    }else if(name==='ListMark'){
      if(!/^\d/.test(state.sliceDoc(from,to))&&!editing(from,to))ranges.push(Decoration.replace({widget:new Bullet()}).range(from,to));
    }else if(name==='Blockquote'){
      for(let pos=state.doc.lineAt(from).from;pos<=to;){line(pos,'cm-md-quote');const next=state.doc.lineAt(pos).to+1;if(next>state.doc.length)break;pos=next;}
    }else if(name==='FencedCode'||name==='IndentedCode'){
      for(let pos=state.doc.lineAt(from).from;pos<to;){line(pos,'cm-md-code-line');const next=state.doc.lineAt(pos).to+1;if(next>state.doc.length)break;pos=next;}
      return false;
    }
  }});
  return Decoration.set(ranges,true);
}
const livePreview=StateField.define({create:previewDecorations,update:(_value,tr)=>previewDecorations(tr.state),provide:field=>EditorView.decorations.from(field)});

// Mobile keyboards can shrink and pan the visual viewport without resizing
// 100dvh. Keep the focused composer inside that visible rectangle. Leave pinch
// zoom to the browser, and restore normal layout when focus leaves the composer.
function setupKeyboardViewport({area,host,view}){
  const viewport=window.visualViewport;
  if(!viewport)return;
  const root=document.documentElement,form=area.closest('form');
  let frame=0;
  const update=()=>{
    frame=0;
    const active=matchMedia('(max-width:800px)').matches&&form.contains(document.activeElement)&&Math.abs(viewport.scale-1)<0.01;
    if(!active){
      root.removeAttribute('data-composer-viewport');
      root.style.removeProperty('--composer-viewport-height');root.style.removeProperty('--composer-viewport-top');
      return;
    }
    const height=Math.round(viewport.height),top=Math.round(viewport.offsetTop);
    if(height<=0)return;
    root.setAttribute('data-composer-viewport','');
    root.style.setProperty('--composer-viewport-height',height+'px');root.style.setProperty('--composer-viewport-top',top+'px');
    view.requestMeasure();
    if(view.hasFocus)view.dispatch({effects:EditorView.scrollIntoView(view.state.selection.main.head,{y:'nearest'})});

  };
  const schedule=()=>{if(!frame)frame=requestAnimationFrame(update);};
  viewport.addEventListener('resize',schedule);viewport.addEventListener('scroll',schedule);
  window.addEventListener('resize',schedule);
  document.addEventListener('focusin',schedule);document.addEventListener('focusout',schedule);
  // A focused editor may be hidden by navigation without receiving a viewport event.
  new MutationObserver(schedule).observe(host,{attributes:true,attributeFilter:['hidden']});
}

export function setupMarkdownComposer({area,host,toggle,storage}){
  let enabled=false,updating=false,scope=null,source=false;
  try{source=storage.getItem('agentworks-composer-source')==='true';}catch{}
  const controls=new Compartment(),preview=new Compartment(),undoHistory=new Compartment();
  const nativeFocus=area.focus.bind(area);
  const mirrorSelection=view=>{const {from,to}=view.state.selection.main;area.setSelectionRange(from,to);};
  const state=(text,selection=0)=>EditorState.create({doc:text,selection:{anchor:Math.min(selection,text.length)},extensions:[
    markdown(),undoHistory.of(history()),EditorView.lineWrapping,preview.of(livePreview),
    controls.of([EditorView.editable.of(!area.disabled),EditorState.readOnly.of(area.disabled),placeholder(area.placeholder)]),
    EditorView.contentAttributes.of({'aria-label':'Message your agent','aria-multiline':'true',spellcheck:'true',autocapitalize:'sentences'}),
    EditorState.transactionFilter.of(tr=>tr.docChanged&&tr.newDoc.length>area.maxLength?[]:tr),
    Prec.highest(EditorView.domEventHandlers({
      keydown(event){if(updating)return false;mirrorSelection(view);area.onkeydown?.(event);return event.defaultPrevented;},
      click(){mirrorSelection(view);area.dispatchEvent(new Event('click'));},
      blur(){area.dispatchEvent(new Event('blur'));},
    })),
    keymap.of([{key:'Shift-Enter',run:view=>insertNewlineContinueMarkup(view)||insertNewlineAndIndent(view)},...historyKeymap,...defaultKeymap]),
    EditorView.updateListener.of(update=>{
      if(updating)return;
      if(update.docChanged||update.selectionSet){
        updating=true;area.value=update.state.doc.toString();mirrorSelection(update.view);
        if(update.docChanged)area.dispatchEvent(new Event('input',{bubbles:true}));
        else if(update.view.hasFocus)area.dispatchEvent(new Event('click'));
        updating=false;
      }
    }),
  ]});
  const view=new EditorView({state:state(area.value),parent:host});
  setupKeyboardViewport({area,host,view});
  let disabled=area.disabled,hint=area.placeholder;
  function sync({markdown:useMarkdown,sessionId}={markdown:enabled,sessionId:scope}){
    const changedScope=scope!==sessionId;scope=sessionId;
    const shouldEnable=!!useMarkdown&&!source;
    const hadFocus=view.hasFocus||document.activeElement===area;
    enabled=!!useMarkdown;
    toggle.hidden=!enabled;toggle.textContent=source?'Formatted':'Source';
    toggle.setAttribute('aria-label',source?'Use formatted Markdown editor':'Edit Markdown source');toggle.setAttribute('aria-pressed',String(source));
    host.hidden=!shouldEnable;area.hidden=shouldEnable;
    view.contentDOM.setAttribute('enterkeyhint',area.getAttribute('enterkeyhint')||'enter');
    if(!updating){
      const text=area.value;
      if(changedScope)view.setState(state(text,area.selectionStart));
      else if(!text&&view.state.doc.length){
        // Clear the sent draft through the normal input path. Recreating the
        // editor here disconnects some mobile keyboards from their target.
        view.dispatch({changes:{from:0,to:view.state.doc.length,insert:''},selection:{anchor:0},annotations:Transaction.addToHistory.of(false),effects:undoHistory.reconfigure([])});
        view.dispatch({effects:undoHistory.reconfigure(history())});
      }
      else if(text!==view.state.doc.toString())view.dispatch({changes:{from:0,to:view.state.doc.length,insert:text},selection:{anchor:area.selectionStart,head:area.selectionEnd},annotations:Transaction.addToHistory.of(false)});
      if(disabled!==area.disabled||hint!==area.placeholder){disabled=area.disabled;hint=area.placeholder;view.dispatch({effects:controls.reconfigure([EditorView.editable.of(!disabled),EditorState.readOnly.of(disabled),placeholder(hint)])});}
    }
    if(hadFocus){if(shouldEnable&&!view.hasFocus)view.focus();else if(!shouldEnable&&document.activeElement!==area)nativeFocus();}
  }
  area.focus=options=>{if(enabled&&!source&&!area.disabled)view.focus();else nativeFocus(options);};
  area.addEventListener('input',()=>{if(!updating)sync();});
  toggle.onclick=()=>{
    source=!source;try{storage.setItem('agentworks-composer-source',String(source));}catch{}
    sync();area.focus();
  };
  return {sync,hasFocus:()=>view.hasFocus||document.activeElement===area};
}
