// Exercise the shipped capability parser/router without a CLI or model process.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';
import {createRequire} from 'node:module';
const {zClientCapabilities}=await import(new URL('./schema/zod.gen.js',import.meta.resolve('@agentclientprotocol/sdk')));
import {subagentForm} from '@agentclientprotocol/claude-agent-acp/dist/acp-subagents.js';
const caps={subagents:{}};
assert.equal(subagentForm(zClientCapabilities.parse(caps)),'rfd');
assert.equal(subagentForm(zClientCapabilities.parse({})),undefined);
const bundle=readFileSync(new URL('../vendor/codex-acp/dist/index.js',import.meta.url),'utf8');
const schema=bundle.slice(0,bundle.indexOf('var zInitializeRequest = object({'))
  .replace(/^#!.*\n/,'').replace(/^import .*\n/gm,'');
const sandbox={require:createRequire(import.meta.url),process,Buffer,console,URL,TextEncoder,TextDecoder};
vm.createContext(sandbox);vm.runInContext(schema,sandbox);
const router=bundle.slice(bundle.indexOf('function clientSupportsSubagents('),bundle.indexOf('// src/CodexHookTrust.ts'));
vm.runInContext(router,sandbox);
const parsed=sandbox.zClientCapabilities.parse(caps);
assert.equal(sandbox.clientSupportsSubagents(parsed),true,'canonical capability survives bundled SDK parser');
assert.equal(sandbox.zClientCapabilities.parse({}).subagents,undefined);
assert.equal(sandbox.zClientCapabilities.parse({subagents:'invalid'}).subagents,undefined);
console.log('subagent adapter negotiation: Codex and Claude passed (no inference)');
