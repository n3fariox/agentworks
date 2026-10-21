import {readFileSync} from 'node:fs';
// Exercise the private production functions without adding a shipped asset or parser API.
const source=readFileSync(new URL('../../web/app.js',import.meta.url),'utf8');
const start=source.indexOf('// U-05 tool activity.');
const end=source.indexOf('// End U-05 tool activity.');
if(start<0||end<start)throw new Error('Missing tool activity implementation');
export const activitySource=source.slice(start,end);
export const {applyToolUpdate,applyToolProgress,toolActivityView,toolRawText}=new Function(activitySource+'\nreturn {applyToolUpdate,applyToolProgress,toolActivityView,toolRawText};')();
export const events=JSON.parse(readFileSync(new URL('./bazel-events.json',import.meta.url),'utf8'));
// Golden `tool_progress` events the service publishes for `events`; regenerated and
// checked by the Rust test tool_progress::tests::u05_fixture_matches_golden_progress_events.
export const progressEvents=JSON.parse(readFileSync(new URL('./bazel-progress-events.json',import.meta.url),'utf8'));
export const build=()=>({id:'build',title:'bazel test //fixture:all',rawInput:{command:'bazel test //fixture:all'},status:'in_progress'});
export const delta=(data,id='term-build')=>({_meta:{terminal_output_delta:{terminal_id:id,data}}});
export const snapshot=(data,id='term-build')=>({_meta:{terminal_output:{terminal_id:id,data}}});
export const view=(phase,counts,extra={})=>({parser:'Bazel',progress:phase?{phase,label:null,counts:counts.map(([done,total,unit])=>({done,total,unit}))}:null,parserFailure:null,...extra});
