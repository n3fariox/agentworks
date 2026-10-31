import {Viewer,ThreeModelLoader,ImportSettings,InputFilesFromFileObjects,RGBAColor,RGBColor,EdgeSettings,EnvironmentSettings} from 'online-3d-viewer';
import {collectModelFiles,companionPath} from './model-files.js';
import {createAlbedoMode} from './model-albedo.js';
import {localState} from './hosting.js';

const viewport=document.getElementById('viewport'),status=document.getElementById('status');
const fit=document.getElementById('fit'),edges=document.getElementById('edges'),albedo=document.getElementById('albedo');
try{albedo.setAttribute('aria-pressed',String(localState.getItem('agentworks-model-albedo')==='true'));}catch{}
let viewer,loader,materialMode,disposed=false;
const showError=error=>{status.textContent='Unable to preview this model: '+(error.message||error)+'. Use Download to open it locally.';};
window.addEventListener('error',event=>showError(event.error||event.message));
window.addEventListener('unhandledrejection',event=>showError(event.reason));
function fitModel(){const sphere=viewer.GetBoundingSphere(()=>true);if(sphere){viewer.AdjustClippingPlanesToSphere(sphere);viewer.FitSphereToWindow(sphere,false);}}
fit.onclick=fitModel;
edges.onclick=()=>{const enabled=edges.getAttribute('aria-pressed')!=='true';edges.setAttribute('aria-pressed',String(enabled));viewer.SetEdgeSettings(new EdgeSettings(enabled,new RGBColor(25,30,38),1));};
albedo.onclick=()=>{
  const enabled=albedo.getAttribute('aria-pressed')!=='true';albedo.setAttribute('aria-pressed',String(enabled));
  try{localState.setItem('agentworks-model-albedo',String(enabled));}catch{}
  materialMode.setEnabled(enabled);viewer.Render();
};
async function start(){
  const {url,path}=JSON.parse(decodeURIComponent(location.hash.slice(1)));
  const target=new URL(url),base=new URL('./',location.href);
  if(target.origin!==location.origin||!target.pathname.startsWith(base.pathname+'api/')||!/^api\/(?:sessions|locations)\/[^/]+\/file$/.test(target.pathname.slice(base.pathname.length)))throw new Error('Invalid model file URL');
  const canvas=document.createElement('canvas');viewport.append(canvas);
  viewer=new Viewer();viewer.Init(canvas);viewer.SetBackgroundColor(new RGBAColor(23,26,30,255));
  // Metallic glTF materials need reflected environment lighting.
  viewer.SetEnvironmentMapSettings(new EnvironmentSettings(['posx','negx','posy','negy','posz','negz'].map(face=>new URL(`./model-viewer-assets/environment/${face}.jpg`,document.baseURI).href),false));
  loader=new ThreeModelLoader();
  const resize=new ResizeObserver(()=>{if(!disposed)viewer.Resize(viewport.clientWidth,viewport.clientHeight);});resize.observe(viewport);
  viewer.Resize(viewport.clientWidth,viewport.clientHeight);
  window.addEventListener('pagehide',()=>{disposed=true;resize.disconnect();materialMode?.dispose();loader.Destroy();viewer.Destroy();},{once:true});
  const model=await collectModelFiles(url,path);
  async function load(){
    if(disposed)return;
    return new Promise(resolve=>loader.LoadModel(InputFilesFromFileObjects(model.files),new ImportSettings(),{
      onLoadStart:()=>{status.textContent='Loading model…';},onFileListProgress:()=>{},onFileLoadProgress:()=>{},
      onImportStart:()=>{status.textContent='Importing model…';},onVisualizationStart:()=>{status.textContent='Preparing preview…';},
      onTextureLoaded:()=>{if(!disposed)viewer.Render();},
      onModelFinished:(result,object)=>resolve({result,object}),
      onLoadError:error=>resolve({error}),
    }));
  }
  let loaded;
  // Discover companion files exposed by other importers, and retry with those files.
  for(let attempt=0;attempt<4;attempt++){
    loaded=await load();if(disposed||!loaded)return;
    if(attempt===3)break;
    const before=model.files.length;
    for(const name of loader.GetImporter().missingFiles){const next=companionPath(path,name);if(next!==null)await model.add(next);}
    if(model.files.length===before)break;
    if(loaded.object){viewer.SetMainObject(loaded.object);viewer.Clear();}
  }
  if(disposed)return;
  if(loaded.error)throw new Error(loaded.error.message||'The importer could not read the model');
  viewer.SetMainObject(loaded.object);materialMode=createAlbedoMode(loaded.object);
  materialMode.setEnabled(albedo.getAttribute('aria-pressed')==='true');
  fitModel();fit.disabled=edges.disabled=albedo.disabled=false;
  const missing=[...new Set([...model.missing,...loaded.result.missingFiles])];
  status.textContent=missing.length?'Some companion files could not be loaded; materials or geometry may be incomplete.':'';
  document.body.dataset.modelLoaded='true';
}
start().catch(showError);
