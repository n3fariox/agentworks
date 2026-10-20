// Deterministic L-02 checks: node tests/questions.test.mjs (no server, no model calls).
import assert from 'node:assert/strict';
import {formFields,initialValues,collect,submissionFor} from '../web/questions.js';

const request={mode:'form',message:'Setup?',requestedSchema:{type:'object',properties:{
  db:{type:'string',title:'Database',oneOf:[{const:'Postgres',title:'Postgres',description:'Server'},{const:'SQLite',title:'SQLite'}]},
  other:{type:'string',title:'Other',maxLength:5},
  legacy:{type:'string',enum:['a','b']},
  features:{type:'array',title:'Features',items:{anyOf:[{const:'auth',title:'Auth'},{const:'search',title:'Search'}]},maxItems:1},
  replicas:{type:'integer',title:'Replicas',minimum:1,maximum:5},
  ratio:{type:'number',title:'Ratio'},
  email:{type:'string',title:'Email',format:'email'},
  when:{type:'string',title:'When',format:'date-time'},
  day:{type:'string',title:'Day',format:'date'},
  code:{type:'string',title:'Code',pattern:'^[A-Z]{3}$'},
  sample:{type:'boolean',title:'Sample',default:true},
  owner:{type:'object',title:'Owner'}},
  required:['db','replicas']}};

const fields=formFields(request);
const byName=Object.fromEntries(fields.map(f=>[f.name,f]));
// Field kinds follow the ACP property schema, in the agent's order.
assert.deepEqual(fields.map(f=>f.kind),['single','text','single','multi','integer','number','text','text','text','text','boolean','unsupported']);
assert.equal(byName.db.required,true);
assert.deepEqual(byName.db.options[0],{value:'Postgres',title:'Postgres',description:'Server'});
assert.deepEqual(byName.legacy.options.map(o=>o.value),['a','b']);
assert.equal(byName.features.maxItems,1);

const init=initialValues(fields);
assert.equal(init.sample,true);
assert.deepEqual(init.features,[]);
assert.equal(init.db,'');

const usable=fields.filter(f=>f.kind!=='unsupported');
// Required fields are reported; empty optional fields are omitted, not sent as "".
let r=collect(usable,init);
assert.deepEqual(Object.keys(r.errors).sort(),['db','replicas']);
assert.equal(r.errors.db,'Database is required');
r=collect(usable,{...init,db:'SQLite',replicas:' 3 '});
assert.deepEqual(r.errors,{});
assert.deepEqual(r.content,{db:'SQLite',replicas:3,sample:true});

// Every constraint has a field-level message.
r=collect(usable,{...init,db:'MySQL',other:'toolong',legacy:'c',features:['auth','search'],replicas:'2.5',ratio:'x',
  email:'a@b',when:'not a date',day:'2023-02-29',code:'abc',sample:false});
assert.deepEqual(Object.keys(r.errors).sort(),['code','day','db','email','features','legacy','other','ratio','replicas','when']);
assert.match(r.errors.features,/at most 1 option$/);
assert.match(r.errors.replicas,/whole number/);
assert.equal(collect(usable,{...init,db:'SQLite',replicas:'9'}).errors.replicas,'Replicas must be at most 5');

// Valid values are normalized to what the server accepts (ISO date-time, numbers, arrays).
r=collect(usable,{...init,db:'Postgres',other:'été',features:['search','bogus'],replicas:'5',ratio:'-0.5',email:'me@example.com',
  when:'2026-10-20T10:30',day:'2024-02-29',code:'ABC',sample:false});
assert.deepEqual(r.errors,{});
assert.deepEqual(r.content.features,['search']);
assert.equal(r.content.other,'été');
assert.equal(r.content.ratio,-0.5);
assert.match(r.content.when,/^2026-10-2\dT\d\d:30:00\.000Z$/);
assert.equal(r.content.sample,false);
assert.equal(collect([byName.other],{other:'ééééé'}).errors.other,undefined,'length counts characters, not bytes');

// Retrying an unchanged answer reuses its submission ID; a changed answer gets a new one.
let n=0;const uuid=()=>`id-${++n}`;const draft={};
const first=submissionFor(draft,{action:'accept',content:{db:'SQLite'}},uuid);
assert.equal(submissionFor(draft,{action:'accept',content:{db:'SQLite'}},uuid),first);
assert.notEqual(submissionFor(draft,{action:'accept',content:{db:'Postgres'}},uuid),first);
assert.notEqual(submissionFor(draft,{action:'decline'},uuid),first);
// The ID survives a reload because the draft is serialized with it.
const restored=JSON.parse(JSON.stringify(draft));
assert.equal(submissionFor(restored,{action:'decline'},uuid),draft.sent.id);
console.log('questions: ok');
