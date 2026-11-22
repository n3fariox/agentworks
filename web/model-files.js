// Keep real filenames when loading models through the query-based file API.
// The viewer otherwise mistakes /file?path=part.stl for a file named "file".
export const modelFileName=path=>path.replaceAll('\\','/').split('/').at(-1);

export function companionPath(path,reference){
  if(!reference||/^(?:[a-z][a-z\d+.-]*:|[/\\])/i.test(reference))return null;
  try{reference=decodeURIComponent(reference);}catch{return null;}
  if(reference.includes('\0'))return null;
  path=path.replaceAll('\\','/');reference=reference.replaceAll('\\','/');
  return path.slice(0,path.lastIndexOf('/')+1)+reference;
}

export function modelReferences(path,bytes){
  const extension=modelFileName(path).split('.').at(-1).toLowerCase();
  const decode=value=>new TextDecoder().decode(value);
  try{
    if(extension==='gltf'||extension==='glb'){
      let text;
      if(extension==='glb'){
        const view=new DataView(bytes);
        if(view.getUint32(0,true)!==0x46546c67||view.getUint32(16,true)!==0x4e4f534a)return [];
        text=decode(bytes.slice(20,20+view.getUint32(12,true)));
      }else text=decode(bytes);
      const json=JSON.parse(text);return [...(json.buffers||[]),...(json.images||[])].map(item=>item.uri).filter(Boolean);
    }
    const text=extension==='obj'||extension==='mtl'?decode(bytes):'';
    if(extension==='obj')return [...text.matchAll(/^\s*mtllib\s+(.+?)\s*$/gmi)].map(match=>match[1]);
    if(extension==='mtl')return [...text.matchAll(/^\s*(?:map_\w+|bump|disp|decal|norm)\s+(.+?)\s*$/gmi)].map(match=>{
      // Common numeric options precede the filename; preserve spaces in filenames.
      return match[1].replace(/^(?:(?:-(?:s|o|t)\s+(?:[-+.\d]+\s+){1,3})|(?:-\w+\s+\S+\s+))+/,'');
    });
  }catch{/* The importer reports malformed models. */}
  return [];
}

export async function collectModelFiles(url,path,fetchFile=fetch){
  const files=[],seen=new Set(),missing=[];let total=0;
  async function add(path,required=false){
    // Canonical keys prevent references such as ./part.mtl from loading twice.
    const key=new URL(path.replaceAll('\\','/'),'file:///').href;
    if(seen.has(key))return;seen.add(key);
    if(seen.size>64)throw new Error('Model references more than 64 files. Export as GLB to preview it.');
    const target=new URL(url);target.searchParams.set('path',path);
    let response;
    try{response=await fetchFile(target);}catch(error){if(required)throw error;missing.push(path);return;}
    if(!response.ok){if(required)throw new Error(await response.text());missing.push(path);return;}
    const bytes=await response.arrayBuffer();total+=bytes.byteLength;
    if(total>64*1024*1024)throw new Error('Model and companion files exceed the 64 MB preview limit.');
    files.push(new File([bytes],modelFileName(path)));
    for(const reference of modelReferences(path,bytes)){
      const next=companionPath(path,reference);if(next!==null)await add(next);
    }
  }
  await add(path,true);
  return {files,missing,add};
}
