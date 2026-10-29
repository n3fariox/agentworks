// Shared rendering of stored badges from hook setters and native producers.
// The browser only renders bounded plain data.
const FRESH_MS=60000;
function badgeUrl(value){
  if(typeof value!=='string'||value.length>2048||!value.startsWith('https://')||/[\s\x00-\x1f\x7f]/.test(value))return null;
  try{const url=new URL(value);return url.protocol==='https:'&&!url.username&&!url.password?url.href:null;}catch{return null;}
}
export function sessionBadges(context,now=Date.now()) {
  const fresh=!context?.badgesUnavailable&&Number.isFinite(context?.badgesAt)&&now-context.badgesAt<=120000;
  return (Array.isArray(context?.badges)?context.badges:[]).filter(b=>typeof b.label==='string'&&b.label.trim()&&b.label.length<=64).slice(0,32).map(b=>({
    ...b,url:badgeUrl(b.url),tone:fresh&&['neutral','success','warning','danger','info'].includes(b.tone)?b.tone:'neutral',
    title:fresh?(b.tooltip||b.label):`${b.tooltip||b.label} · Status unavailable or out of date`,
  }));
}
export function setupSessionBadges({api,area=document.getElementById('sessions')}) {
  const cache=new Map(),requests=new Map(),queue=[],queued=new Set(),visible=new Set();
  let active=0,epoch=0;
  const rows=()=>[...area.querySelectorAll('.session-badges')];
  const rowId=node=>node.closest('.session-item')?.dataset.sessionId;
  function paint(id){
    const badges=sessionBadges(cache.get(id)?.data);
    for(const row of rows().filter(n=>rowId(n)===id)){
      row.replaceChildren();
      for(const data of badges){
        const badge=document.createElement(data.url?'a':'span');badge.className=`session-badge ${data.tone}`;badge.textContent=data.label;badge.title=data.title;badge.setAttribute('aria-label',data.title);
        if(data.url){badge.href=data.url;badge.target='_blank';badge.rel='noopener noreferrer';}else badge.setAttribute('role','img');row.append(badge);
      }
    }
  }
  async function read(id,refresh=false){
    const saved=cache.get(id),minAge=refresh?10000:FRESH_MS;
    if(saved&&Date.now()-saved.at<minAge)return saved.data;
    if(requests.has(id))return requests.get(id);
    const version=epoch;
    let request;request=(async()=>{
      try{
        const data=await api(`/api/sessions/${encodeURIComponent(id)}/github${refresh?'?refresh=true':''}`);
        if(version===epoch){
          const previous=cache.get(id)?.data;
          let display=!Array.isArray(data.badges)&&['unavailable','workspace_unavailable'].includes(data.status)&&previous?{...data,snapshot:previous.snapshot,badges:previous.badges,badgesAt:previous.badgesAt,badgesUnavailable:true}:data;
          if((previous?.badgeRevision??-1)>(display.badgeRevision??-1))display={...display,badges:previous.badges,badgesAt:previous.badgesAt,badgeRevision:previous.badgeRevision,badgesUnavailable:false};
          if(cache.size>=250&&!cache.has(id))cache.delete(cache.keys().next().value);
          cache.set(id,{at:Date.now(),data:display});paint(id);
        }
        return data;
      }catch(error){
        if(version===epoch){cache.set(id,{at:Date.now(),data:{...cache.get(id)?.data,status:'unavailable',badgesUnavailable:true}});paint(id);}
        throw error;
      }finally{if(requests.get(id)===request)requests.delete(id);}
    })();
    requests.set(id,request);return request;
  }
  function drain(){
    while(active<2&&queue.length){
      const id=queue.shift();queued.delete(id);
      if(!visible.has(id))continue;
      active++;read(id).catch(()=>{}).finally(()=>{active--;drain();});
    }
  }
  function enqueue(id){if(!queued.has(id)){queue.push(id);queued.add(id);}drain();}
  const observer=new IntersectionObserver(entries=>{
    for(const entry of entries){const id=entry.target.dataset.sessionId;
      if(entry.isIntersecting&&!document.hidden){visible.add(id);enqueue(id);}else visible.delete(id);
    }
  });
  function sync(){
    observer.disconnect();visible.clear();queue.length=0;queued.clear();
    for(const row of rows()){paint(rowId(row));observer.observe(row.closest('.session-item'));}
  }
  function invalidate(){epoch++;cache.clear();requests.clear();sync();}
  setInterval(()=>{if(!document.hidden)for(const id of visible)enqueue(id);},FRESH_MS);
  document.addEventListener('session-badges-changed',invalidate);
  document.addEventListener('visibilitychange',()=>{if(!document.hidden)sync();});
  function update(id,payload){
    const previous=cache.get(id);if((previous?.data.badgeRevision??-1)>(payload.badgeRevision??-1))return;
    cache.set(id,{at:previous?.at??0,data:{...previous?.data,...payload,badgesUnavailable:false}});
    paint(id);
  }
  return {read,sync,invalidate,update};
}
