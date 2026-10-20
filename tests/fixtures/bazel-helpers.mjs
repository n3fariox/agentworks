import {readFileSync} from 'node:fs';
// Exercise the private production functions without adding a shipped asset or parser API.
const source=readFileSync(new URL('../../web/app.js',import.meta.url),'utf8');
const start=source.indexOf('// U-05 built-in Bazel activity.');
const end=source.indexOf('// End U-05 built-in Bazel activity.');
if(start<0||end<start)throw new Error('Missing built-in activity implementation');
export const activitySource=source.slice(start,end);
export const {applyToolUpdate,toolActivityView,toolRawText}=new Function(activitySource+'\nreturn {applyToolUpdate,toolActivityView,toolRawText};')();
export const events=JSON.parse(readFileSync(new URL('./bazel-events.json',import.meta.url),'utf8'));
export const build=()=>({id:'build',title:'bazel test //fixture:all',rawInput:{command:'bazel test //fixture:all'},status:'in_progress'});
export const delta=(data,id='term-build')=>({_meta:{terminal_output_delta:{terminal_id:id,data}}});
export const snapshot=(data,id='term-build')=>({_meta:{terminal_output:{terminal_id:id,data}}});
