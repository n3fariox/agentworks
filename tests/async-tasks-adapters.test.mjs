// Verify installed adapter contracts without paid inference.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';
import {createRequire} from 'node:module';
import {clientSupportsAsyncTasks} from '@agentclientprotocol/claude-agent-acp/dist/async-tasks.js';
import {subagentForm} from '@agentclientprotocol/claude-agent-acp/dist/acp-subagents.js';
const {zClientCapabilities}=await import(new URL('./schema/zod.gen.js',import.meta.resolve('@agentclientprotocol/sdk')));
const caps={subagents:{},_meta:{terminal_output:true,terminal_output_delta:true,jetbrains:{air:{version:1,capabilities:['asyncTasks']}}}};
assert.equal(clientSupportsAsyncTasks(zClientCapabilities.parse(caps)),true);
assert.equal(clientSupportsAsyncTasks({}),false);
assert.equal(subagentForm(zClientCapabilities.parse(caps)),'air');
const bundle=readFileSync(new URL('../vendor/codex-acp/dist/index.js',import.meta.url),'utf8');
const schema=bundle.slice(0,bundle.indexOf('var zInitializeRequest = object({')).replace(/^#!.*\n/,'').replace(/^import .*\n/gm,'');
const sandbox={require:createRequire(import.meta.url),process,Buffer,console,URL,TextEncoder,TextDecoder};
vm.createContext(sandbox);vm.runInContext(schema,sandbox);
// Use the bundled SDK parser, then the adapter's actual capability check.
const start=bundle.indexOf('// src/AirExtension.ts');
const end=bundle.indexOf('// src/',start+1);
vm.runInContext(bundle.slice(start,end),sandbox);
assert.equal(sandbox.clientSupportsAirCapability(sandbox.zClientCapabilities.parse(caps),'asyncTasks'),true);
assert.equal(sandbox.clientSupportsAirCapability(sandbox.zClientCapabilities.parse({}),'asyncTasks'),false);
console.log('asyncTasks negotiation: installed Claude and bundled Codex adapters passed (no inference)');
