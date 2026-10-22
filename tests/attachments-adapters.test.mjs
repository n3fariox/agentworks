// Attachment provider mapping: node tests/attachments-adapters.test.mjs (no server, no model calls).
// Feeds the exact ACP blocks Agentworks sends (see runtime::attachment_block) through the
// pinned adapters' own prompt conversion and checks each reaches a native model input.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';
import {promptToClaude} from '@agentclientprotocol/claude-agent-acp/dist/acp-agent.js';

const uri='agentworks://attachments/00000000-0000-4000-8000-000000000000/notes.md';
const prompt=[
  {type:'text',text:'describe these'},
  {type:'image',data:'iVBORw0KGgo=',mimeType:'image/png'},
  {type:'resource',resource:{uri,mimeType:'text/markdown',text:'# Notes'}},
];

// Claude: native base64 image block plus a <context> text block.
const claude=promptToClaude({sessionId:'s',prompt}).message.content;
assert.deepEqual(claude[1],{type:'image',source:{type:'base64',data:'iVBORw0KGgo=',media_type:'image/png'}});
assert.ok(claude.some(b=>b.type==='text'&&b.text.includes(`<context ref="${uri}">\n# Notes\n</context>`)));
// A binary blob resource is silently dropped by Claude, which is why non-image files are refused.
assert.equal(promptToClaude({sessionId:'s',prompt:[{type:'resource',resource:{uri,mimeType:'application/pdf',blob:'JVBERg=='}}]}).message.content.length,0);

// Codex: evaluate the vendored (tracked) conversion functions in isolation.
const source=readFileSync(new URL('../vendor/codex-acp/dist/index.js',import.meta.url),'utf8');
const pick=name=>{const start=source.indexOf(`\nfunction ${name}(`);assert.ok(start>0,name);const end=source.indexOf('\n}\n',start);return source.slice(start,end+3);};
const context=vm.createContext({URL});
vm.runInContext(['buildPromptItems','imageDataUrl','isImageMimeType','isSupportedImageUrl','formatUriAsLink'].map(pick).join('\n'),context);
const codex=context.buildPromptItems(prompt);
assert.deepEqual(JSON.parse(JSON.stringify(codex[1])),{type:'image',url:'data:image/png;base64,iVBORw0KGgo='});
assert.ok(codex[2].type==='text'&&codex[2].text.includes(`<context ref="${uri}">\n# Notes\n</context>`));
// Codex would dump a non-image blob as base64 text, which a model cannot read as a document.
assert.match(context.buildPromptItems([{type:'resource',resource:{uri,mimeType:'application/pdf',blob:'JVBERg=='}}])[0].text,/encoding="base64"/);
console.log('attachment adapter mapping: ok');
