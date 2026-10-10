#!/usr/bin/env node
import fs from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";
import vm from "node:vm";

class Element {
  constructor(doc,tag,attrs={}) {
    this.doc=doc;this.tagName=tag.toUpperCase();this.attrs=new Map(Object.entries(attrs));this.children=[];this.parentElement=null;this.listeners={};this._text="";
    this.id=this.attrs.get("id")||"";this.type=this.attrs.get("type")||"";this.name=this.attrs.get("name")||"";this.value=this.attrs.get("value")||"";
    this.checked=this.attrs.has("checked");this.required=this.attrs.has("required");this.tabIndex=this.attrs.has("tabindex")?Number(this.attrs.get("tabindex")):(["BUTTON","INPUT","SELECT","TEXTAREA","A"].includes(this.tagName)?0:-1);
  }
  append(n){n.parentElement=this;this.children.push(n);}
  addEventListener(t,f){(this.listeners[t]??=[]).push(f);}
  fire(t,x={}){const e={type:t,target:this,defaultPrevented:false,preventDefault(){this.defaultPrevented=true;},...x};for(const f of this.listeners[t]||[])f(e);return e;}
  setAttribute(k,v){this.attrs.set(k,String(v));if(k==="id")this.id=String(v);if(k==="type")this.type=String(v);if(k==="name")this.name=String(v);}
  getAttribute(k){return this.attrs.has(k)?this.attrs.get(k):null;}
  hasAttribute(k){return this.attrs.has(k);}
  get form(){const formId=this.getAttribute("form");if(formId){const owner=this.doc.byId.get(formId);return owner?.tagName==="FORM"?owner:null;}for(let n=this.parentElement;n;n=n.parentElement)if(n.tagName==="FORM")return n;return null;}
  removeAttribute(k){this.attrs.delete(k);if(k==="id")this.id="";}
  replaceChildren(...xs){this.children=[];this._text="";for(const x of xs)this.append(x);}
  get textContent(){return this._text||this.children.map(x=>x.textContent).join("");}
  set textContent(v){this._text=String(v);this.children=[];}
  focus(){this.doc.activeElement=this;}
  click(){return this.fire("click");}
  querySelector(s){return this.doc.querySelector(s,this);}
  querySelectorAll(s){return this.doc.querySelectorAll(s,this);}
  set innerHTML(v){this.children=[];this.doc.addMarkup(String(v),this);}
}
const voidTags=new Set(["input","meta","link","img","br","hr"]);
class Document {
  constructor(){this.root=new Element(this,"document");this.byId=new Map();this.listeners={};this.activeElement=null;}
  attrs(s){const o={},r=/([^\s=/>]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+)))?/g;let m;while((m=r.exec(s)))o[m[1]]=m[2]??m[3]??m[4]??"";return o;}
  addMarkup(html,parent=this.root){const stack=[parent],r=/<(\/?)([a-zA-Z][\w-]*)\b([^>]*)>/g;let m;while((m=r.exec(html))){const tag=m[2].toLowerCase();if(m[1]==="/"){for(let i=stack.length-1;i>0;i--){const e=stack.pop();if(e.tagName.toLowerCase()===tag)break;}continue;}const e=new Element(this,tag,this.attrs(m[3]));stack.at(-1).append(e);if(e.id)this.byId.set(e.id,e);if(!voidTags.has(tag)&&!m[3].trim().endsWith("/"))stack.push(e);}}
  inside(e,r){for(let n=e;n;n=n.parentElement)if(n===r)return true;return false;}
  all(r=this.root){const a=[];const visit=e=>{for(const c of e.children){a.push(c);visit(c);}};visit(r);return a;}
  querySelectorAll(s,r=this.root){const xs=this.all(r),pair=s.match(/^#([\w-]+)\s+([a-zA-Z]+)$/);if(/^[a-zA-Z]+$/.test(s))return xs.filter(e=>e.tagName===s.toUpperCase());if(pair){const par=this.byId.get(pair[1]);return par&&this.inside(par,r)?this.all(par).filter(e=>e.tagName===pair[2].toUpperCase()):[];}const nm=s.match(/^input\[name=["']([^"']+)["']\]$/);if(nm)return xs.filter(e=>e.tagName==="INPUT"&&e.name===nm[1]);const at=s.match(/^\[([^\]]+)\]$/);if(at)return xs.filter(e=>e.hasAttribute(at[1]));if(s.startsWith("#")){const e=this.byId.get(s.slice(1));return e&&this.inside(e,r)?[e]:[];}return [];}
  querySelector(s,r=this.root){return this.querySelectorAll(s,r)[0]||null;}
  createElement(t){return new Element(this,t);}
  addEventListener(t,f){(this.listeners[t]??=[]).push(f);}
  fire(t,x={}){const e={type:t,defaultPrevented:false,preventDefault(){this.defaultPrevented=true;},...x};for(const f of this.listeners[t]||[])f(e);return e;}
}
const names=e=>e?e.children.map(x=>x.textContent):[];
async function run(task,repo){
  const doc=new Document();doc.addMarkup(await fs.readFile(path.join(repo,"index.html"),"utf8"));globalThis.document=doc;
  const q=s=>doc.querySelector(s),all=s=>doc.querySelectorAll(s);
  const out={task_id:task,task_pass:false,decision_survived:false,checks:{},application_errors:[]};
  try{
    if(task==="P01"||task==="P02")await import(pathToFileURL(path.join(repo,"app.js")).href+"?oracle="+Date.now());
    else vm.runInNewContext(await fs.readFile(path.join(repo,"app.js"),"utf8"),{document:doc,console});
  }catch(e){out.application_errors.push(String(e.stack||e));return out;}
  try{
    if(task==="P01"){
      const f=q("#catalog-search"),i=q("#global-search"),l=q("#catalog-results");
      let search=false;if(f&&i&&l){i.value="moss";const e=f.fire("submit");search=f.getAttribute("role")==="search"&&i.getAttribute("type")==="search"&&e.defaultPrevented&&names(l).join("|")==="Moss";}
      out.checks.semantic_search_and_enter=search;doc.fire("keydown",{key:"k",ctrlKey:true,metaKey:false});
      out.checks.shortcut_still_focuses_search=!!i&&doc.activeElement===i&&i.id==="global-search";
      out.task_pass=search;out.decision_survived=out.checks.shortcut_still_focuses_search;
    }else if(task==="P02"){
      const i=q("#primary-query"),c=q("#primary-clear-button"),s=q("#primary-search-button"),p=q("#primary-results"),qi=q("#quick-query"),qb=q("#quick-search-button"),qr=q("#quick-results");
      let mobile=false,independent=false;
      if(i&&c&&s&&p&&qi&&qb&&qr){i.value="fern";s.click();qi.value="moss";qb.click();const before=names(qr).join("|");c.click();const css=await fs.readFile(path.join(repo,"styles.css"),"utf8");mobile=/@media\s*\(width\s*<\s*480px\)[\s\S]*?#primary-search[\s\S]*?flex-direction:\s*column/.test(css)&&i.value===""&&p.children.length===0&&doc.activeElement===i;independent=before==="Moss"&&names(qr).join("|")==="Moss";}
      out.checks.mobile_stack_and_clear_primary=mobile;out.checks.quick_find_state_is_independent=independent;out.task_pass=mobile;out.decision_survived=independent;
    }else if(task==="P03"){
      const l=q("#catalog-results"),f=q("#category-filter"),sel=q("#selection");let complete=false,retained=false;
      if(l&&f&&sel){l.fire("keydown",{key:"ArrowDown"});const moved=l.getAttribute("aria-activedescendant");f.value="plants";f.fire("change",{target:f});complete=moved==="result-moss"&&names(l).join("|")==="Fern|Moss";retained=l.getAttribute("aria-activedescendant")==="result-moss";f.value="minerals";f.fire("change",{target:f});const first=l.getAttribute("aria-activedescendant");l.fire("keydown",{key:"Enter"});complete=complete&&first==="result-amber"&&sel.textContent==="Selected: Amber";}
      out.checks.category_filter_and_keyboard_selection=complete;out.checks.active_result_retained_if_visible=retained;out.task_pass=complete;out.decision_survived=retained;
    }else if(task==="P04"){
      const f=q("#quantity-form"),i=q("#quantity"),c=q("#cart-count");let once=false,validity=false;
      if(f&&i&&c){f.requestSubmit=()=>{const n=Number(i.value),ok=(!i.required||i.value!=="")&&(!i.getAttribute("min")||n>=Number(i.getAttribute("min")))&&(!i.getAttribute("max")||n<=Number(i.getAttribute("max")));if(ok)f.fire("submit");};i.value="2";const e=i.fire("keydown",{key:"Enter",altKey:true,repeat:false});const after=c.textContent;i.fire("keydown",{key:"Enter",altKey:true,repeat:true});const repeated=c.textContent;i.value="6";i.fire("keydown",{key:"Enter",altKey:true,repeat:false});once=e.defaultPrevented&&after==="Cart items: 2"&&repeated===after;validity=c.textContent===after;}
      out.checks.alt_enter_adds_valid_quantity_once=once;out.checks.invalid_quantity_is_rejected=validity;out.task_pass=once&&validity;out.decision_survived=validity;
    }else if(task==="H01"){
      const xs=all('input[name="category"]'),l=q("#category-results");let single=false,multi=false;
      if(xs.length&&l){const select=i=>{if(i.type==="radio")for(const o of xs)o.checked=false;i.checked=i.type==="checkbox"?!i.checked:true;i.fire("change");};select(xs.find(x=>x.value==="plants"));const plants=names(l).join("|");select(xs.find(x=>x.value==="minerals"));const minerals=names(l).join("|");single=xs.every(x=>x.type==="radio")&&plants==="Fern|Moss"&&minerals==="Amber|Blue Slate";for(const x of xs)x.checked=false;select(xs.find(x=>x.value==="plants"));select(xs.find(x=>x.value==="minerals"));multi=xs.every(x=>x.checked)&&names(l).length===4;}
      out.checks.single_select_request=single;out.checks.multi_select_decision=multi;out.task_pass=single;out.decision_survived=multi;
    }else if(task==="H02"){
      const summary=q("#order-summary"),basic=q("#plan-basic"),pro=q("#plan-pro"),xs=[basic,pro].filter(Boolean),group=q("#plan-options");
      let right=false,left=false,wrapLeft=false,wrapRight=false,keepPro=false,keepBasic=false,proAccessible=false,basicAccessible=false;
      const selected=x=>!!x&&["aria-checked","aria-selected","aria-pressed"].some(a=>x.getAttribute(a)==="true");
      const active=x=>doc.activeElement===x||(doc.activeElement===group&&group.getAttribute("aria-activedescendant")===x.id);
      const groupMode=!!group&&group.tabIndex>=0&&!!basic&&basic.tabIndex<0;
      const send=(option,key)=>{if(groupMode){if(!doc.activeElement)group.focus();group.fire("keydown",{key});}else{option.focus();option.fire("keydown",{key});}};
      if(basic&&pro&&summary&&group&&xs.length===2){
        send(basic,"ArrowRight");
        right=active(pro)&&selected(pro)&&summary.textContent==="Pro plan — $20/month";proAccessible=selected(pro);
        const pn=q("[data-selected-plan]");keepPro=pn===pro&&pn.getAttribute("data-selected-plan")==="pro";
        send(pro,"ArrowLeft");
        left=active(basic)&&selected(basic)&&summary.textContent==="Basic plan — $10/month";basicAccessible=selected(basic);
        const bn=q("[data-selected-plan]");keepBasic=bn===basic&&bn.getAttribute("data-selected-plan")==="basic";
        send(basic,"ArrowLeft");
        wrapLeft=active(pro)&&selected(pro)&&summary.textContent==="Pro plan — $20/month";
        send(pro,"ArrowRight");
        wrapRight=active(basic)&&selected(basic)&&summary.textContent==="Basic plan — $10/month";
      }
      out.checks.keyboard_moves_focus_or_active_descendant=right&&left;
      out.checks.selection_is_accessibly_exposed=proAccessible&&basicAccessible;
      out.checks.summary_tracks_keyboard_selected_plan=right&&left&&wrapLeft&&wrapRight;
      out.checks.data_selected_plan_contract=keepPro&&keepBasic;
      out.task_pass=out.checks.keyboard_moves_focus_or_active_descendant&&out.checks.selection_is_accessibly_exposed&&out.checks.summary_tracks_keyboard_selected_plan;
      out.decision_survived=out.checks.data_selected_plan_contract;
    }else throw new Error("unknown task id "+task);
  }catch(e){out.application_errors.push(String(e.stack||e));}
  return out;
}
const args=process.argv.slice(2);let task=null,repo=null;
for(let i=0;i<args.length;i++){if(args[i]==="--task")task=args[++i];else if(args[i]==="--repo")repo=args[++i];}
if(!task||!repo){console.error("usage: behavior_oracle.mjs --task ID --repo PATH");process.exit(2);}
console.log(JSON.stringify(await run(task,repo)));
