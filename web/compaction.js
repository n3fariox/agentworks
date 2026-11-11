// ACP context compactions (`compaction_update`, `compaction_summary_chunk`) as log dividers.
// Updates are upserts by compactionId: summary, error and _meta patch, other fields replace.
const fmt=n=>n>=1e6?(n/1e6).toFixed(n>=1e7?0:1)+'M':n>=1e3?(n/1e3).toFixed(n>=1e4?0:1)+'k':String(n);
const TRIGGERS={auto:'automatic',manual:'manual'};

export function applyCompaction(item,d){
  if(d.sessionUpdate==='compaction_summary_chunk'){
    if(d.content?.type==='text')item.summary=(item.summary||'')+d.content.text;
    item.status??='in_progress';return;
  }
  item.status=d.status;
  if('summary' in d)item.summary=(d.summary||[]).filter(b=>b?.type==='text').map(b=>b.text).join('\n')||null;
  if('error' in d)item.error=d.error;
  if(d._meta===null)item.meta={};
  else if(d._meta)for(const [k,v] of Object.entries(d._meta)){if(v===null)delete item.meta[k];else item.meta[k]=v;}
}

// Claude and Goose report trigger and token counts; Codex reports only the status.
export function compactionLabel(item){
  const info=item.meta?.['_claude/compaction']||item.meta?.['_goose/compaction']||{};
  if(item.status==='in_progress')return 'Compacting context…';
  if(item.status==='failed')return `Context compaction failed${item.error?`: ${item.error}`:''}`;
  if(item.status==='cancelled')return 'Context compaction cancelled';
  const parts=['Context compacted'];
  if(TRIGGERS[info.trigger])parts.push(TRIGGERS[info.trigger]);
  if(Number.isFinite(info.preTokens)&&Number.isFinite(info.postTokens))parts.push(`${fmt(info.preTokens)} → ${fmt(info.postTokens)} tokens`);
  return parts.join(' · ');
}

export function renderCompaction(node,item){
  node.dataset.status=item.status||'';
  node.classList.toggle('has-summary',!!item.summary);
  node.querySelector('summary').textContent=compactionLabel(item);
  const body=node.querySelector('.compaction-summary');
  if(body.textContent!==(item.summary||''))body.textContent=item.summary||'';
}
