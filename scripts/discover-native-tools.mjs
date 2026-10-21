// Resolve the same bundled provider CLIs as the ACP adapters. Output paths only.
import {createRequire} from 'node:module';
import {resolve} from 'node:path';
const root=process.argv[2];
const tools={};
try {
  const require=createRequire(resolve(root,'vendor/codex-acp/dist/index.js'));
  tools.codex=process.env.CODEX_PATH||require.resolve('@openai/codex/bin/codex.js');
} catch {}
try {
  const {claudeCliPath}=await import('@agentclientprotocol/claude-agent-acp/dist/acp-agent.js');
  tools.claude=await claudeCliPath();
} catch {}
console.log(JSON.stringify(tools));
