// Served without a session at the app's own URL, so relative paths keep a proxy's mount.
// The app URL names and scopes the session cookie; only the browser knows the mount.
const base=new URL('./',import.meta.url).href;
const $=id=>document.getElementById(id);
$('login-form').addEventListener('submit',async event=>{
  event.preventDefault();$('login-error').textContent='';$('login-submit').disabled=true;
  try{
    const response=await fetch('./api/auth/login',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({username:$('login-username').value,password:$('login-password').value,base})});
    if(response.ok){location.reload();return;}
    $('login-error').textContent=response.status===401?'Sign-in failed.':await response.text();
  }catch{$('login-error').textContent='Cannot reach Agentworks.';}
  $('login-password').value='';$('login-submit').disabled=false;$('login-password').focus();
});
