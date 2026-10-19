// Temporary phone flyouts for less-used controls (composer options, session
// bar actions). Wide layouts show the same elements inline; CSS decides, so
// this only tracks open state. A flyout closes on an outside tap, Escape, or
// after choosing a button inside it, except within [data-flyout-stay] areas
// such as expandable details.
export function setupFlyout({menu,triggers}){
  const set=open=>{menu.classList.toggle('open',open);for(const t of triggers)t.setAttribute('aria-expanded',String(open));};
  const isOpen=()=>menu.classList.contains('open');
  for(const t of triggers)t.addEventListener('click',()=>set(!isOpen()));
  menu.addEventListener('click',event=>{const b=event.target.closest('button');if(b&&!b.closest('[data-flyout-stay]'))set(false);});
  document.addEventListener('pointerdown',event=>{if(isOpen()&&!menu.contains(event.target)&&!triggers.some(t=>t.contains(event.target)))set(false);});
  document.addEventListener('keydown',event=>{if(event.key==='Escape'&&isOpen()&&!document.querySelector('dialog[open]')){set(false);triggers[0].focus();}});
  set(false);
  return {close:()=>set(false),setFlag:on=>triggers[0].classList.toggle('flagged',on)};
}
