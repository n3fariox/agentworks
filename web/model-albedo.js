import {MeshBasicMaterial} from 'three';

// Keep the original materials intact so switching back restores the authored look.
// Textures are shared, not cloned or disposed by this preview override.
export function createAlbedoMode(object){
  const meshes=[],materials=new Map();
  object.traverse(mesh=>{if(mesh.isMesh)meshes.push({mesh,original:mesh.material});});
  function unlit(source){
    if(!materials.has(source))materials.set(source,new MeshBasicMaterial({
      color:source.color?.clone(),map:source.map,alphaMap:source.alphaMap,
      vertexColors:source.vertexColors,transparent:source.transparent,opacity:source.opacity,
      alphaTest:source.alphaTest,side:source.side,depthWrite:source.depthWrite,
      blending:source.blending,premultipliedAlpha:source.premultipliedAlpha,toneMapped:false,
      polygonOffset:source.polygonOffset,polygonOffsetFactor:source.polygonOffsetFactor,polygonOffsetUnits:source.polygonOffsetUnits,
    }));
    return materials.get(source);
  }
  function setEnabled(enabled){
    for(const {mesh,original}of meshes)mesh.material=enabled?(Array.isArray(original)?original.map(unlit):unlit(original)):original;
  }
  return {setEnabled,dispose(){setEnabled(false);for(const material of materials.values())material.dispose();materials.clear();}};
}
