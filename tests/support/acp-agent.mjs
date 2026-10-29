// Wraps a Node ACP fixture as the executable AGENTWORKS_ACP_COMMAND runs in place of the Claude/Codex bridges.
import {writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
export async function acpAgent(dir,fixture){
  const path=join(dir,'acp-agent');
  await writeFile(path,`#!/bin/sh\nexec ${JSON.stringify(process.execPath)} ${JSON.stringify(resolve(fixture))} "$@"\n`,{mode:0o755});
  return path;
}
