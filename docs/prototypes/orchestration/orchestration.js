'use strict';

const $ = (selector, root = document) => root.querySelector(selector);
const $$ = (selector, root = document) => [...root.querySelectorAll(selector)];
const esc = value => String(value).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const STORE = 'superapp-orchestration-draft-v4';
let resetting = false;
const data = {
  files: fixtures().map(f=>({...f,reviewed:f.id!=='tests',changedSinceReview:false})),
  chats: initialChats(), createdWorkspaces:[], sampleWorkspaces:workspaces.map(w=>({...w})), recentChats:{zurich:'implementation'}, defaults:{harness:'Codex',model:'default'}, pendingPr:{}, commentDrafts:{},
  snapshot:4, publishedSnapshot:4, editApplied:false, editStep:null, rebased:false,
  hasPr:true, prDraft:false, checks:'passing', conflict:false, autoMerge:false, merged:false,
  offline:false, comments:[], reviewNotice:'', activity:[],
  terminals:{t1:{id:'t1',workspace:'zurich',output:['❯ cargo test review','12 passed; 0 failed'],draft:''}},
  embeddedTerminals:{zurich:'t1'}, nextTerminal:2,
  otherUnread:['bern']
};
try { const saved=JSON.parse(localStorage.getItem(STORE)||'null'); if(saved?.version===4)Object.assign(data,saved); } catch {}
// A reviewed file is one atomic mark; changed-line counts weight whole files.
const ui = {panels:[], focus:null, nextId:1, scene:'workspaces', pendingEdit:false};
function save(){try{localStorage.setItem(STORE,JSON.stringify({...data,version:4}));}catch{toast('Browser storage unavailable; sample changes last for this visit.');}}
function lines(files=data.files){return files.flatMap(f=>f.hunks.flatMap(h=>h.lines.filter(l=>l.kind!=='context')));}
function counts(files=data.files){return {
  total:lines(files).length,
  reviewed:files.filter(f=>f.reviewed).reduce((n,f)=>n+lines([f]).length,0),
  changed:files.filter(f=>!f.reviewed&&f.changedSinceReview).reduce((n,f)=>n+lines([f]).length,0)
};}
function fileRecord(id){return data.files.find(f=>f.id===id);}
function allWorkspaces(){return [...data.createdWorkspaces,...data.sampleWorkspaces];}
function workspace(id='zurich'){return allWorkspaces().find(w=>w.id===id)||data.sampleWorkspaces[0];}
function workspaceUnread(id){return data.chats.some(c=>c.workspace===id&&c.unread)||data.otherUnread.includes(id);}
function initialChats(){return [
  {id:'implementation',workspace:'zurich',harness:'Codex',model:'default',status:'Ready',unread:true,draft:'',messages:[
    {role:'You',text:'Keep my file review marks across agent changes and rebases.'},
    {role:'Codex',text:'Reviewed files keep their marks through an unchanged rebase. Editing a reviewed file returns the whole file to your review queue.',step:'implementation'}
  ]},
  {id:'reviewer',workspace:'zurich',harness:'Claude Code',model:'Opus',status:'Ready',unread:true,draft:'',messages:[
    {role:'You',text:'Review the matching logic, including duplicated content and rebases.'},
    {role:'Claude Code',text:'Add a test for conflict resolutions during a rebase. Affected files should need review again.',step:'review'}
  ]}
];}
function chatLabel(chat){return `chat ${data.chats.filter(c=>c.workspace===chat.workspace).findIndex(c=>c.id===chat.id)+1}`;}
function chatPreview(chat){return chat.messages.at(-1)?.text||'no messages yet';}
function freshChat(workspaceId,harness=data.defaults.harness,model=harness===data.defaults.harness?data.defaults.model:defaultModel(harness)){
  const chat={id:`chat-${Date.now()}-${data.chats.length}`,workspace:workspaceId,harness,model,status:'Ready',unread:false,draft:'',messages:[]};
  data.chats.push(chat);return chat;
}
function defaultModel(harness){return harness==='Claude Code'?'Opus':'default';}
function freshTerminal(workspaceId){const id=`t${data.nextTerminal++}`;data.terminals[id]={id,workspace:workspaceId,output:[],draft:''};return id;}
function embeddedTerminal(workspaceId){if(!data.embeddedTerminals[workspaceId])data.embeddedTerminals[workspaceId]=freshTerminal(workspaceId);return data.terminals[data.embeddedTerminals[workspaceId]];}

function nodeById(id){return ui.panels.find(p=>p.id===id);}
function node(kind,arg,extra={}){return {id:`p${ui.nextId++}`,kind,arg,parent:null,filter:'',cursor:null,marks:[],...extra};}
function removeChain(id){for(const child of ui.panels.filter(p=>p.parent===id))removeChain(child.id);ui.panels=ui.panels.filter(p=>p.id!==id);}
function closePanel(id){const p=nodeById(id);if(!p)return;removeChain(id);ui.focus=p.parent||ui.panels.at(-1)?.id;render();}
function openPanel(parentId,kind,arg,options={}){
  options={...options,independent:options.independent??ui.nextIndependent??false};
  ui.nextIndependent=false;
  const parent=nodeById(parentId);
  if(!options.independent){const child=ui.panels.find(p=>p.parent===parentId);if(child)removeChain(child.id);}
  const p=node(kind,arg,{...options,parent:options.independent?null:parent?.id||null});
  const index=parent?ui.panels.findIndex(n=>n.id===parentId)+1:ui.panels.length;
  ui.panels.splice(options.independent?ui.panels.length:index,0,p);
  ui.focus=options.keepFocus?parentId:p.id;
  render();
  ensureVisible(p.id,options.keepFocus?parentId:null);
  return p;
}
function ensureVisible(id,keepFocus){
  const el=$(`[data-panel-id="${id}"]`);if(!el)return;
  const stage=$('#stage'),box=el.getBoundingClientRect();
  if(box.right>innerWidth-8)stage.scrollLeft+=box.right-innerWidth+8;
  if(box.left<8)stage.scrollLeft+=box.left-8;
  const focused=$(`[data-panel-id="${keepFocus}"]`);
  if(focused&&focused.getBoundingClientRect().right<80)ui.focus=id;
  syncFocus();
}
function syncFocus(){$$('[data-panel-id]').forEach(el=>el.classList.toggle('focused',el.dataset.panelId===ui.focus));}
function toast(text){$('#toast').textContent=text;$('#toast').classList.add('visible');clearTimeout(toast.timer);toast.timer=setTimeout(()=>$('#toast').classList.remove('visible'),4000);}
function button(label,action,extra=''){return `<button class="action" data-action="${action}" ${extra}>${esc(label)}</button>`;}
function link(label,action,extra=''){return `<button class="link" data-action="${action}" ${extra}>${esc(label)}</button>`;}
function panel(p,title,body,bar='',width=4,classes=''){
  return `<section class="panel width-${width} ${p.parent?'joined':''} ${ui.focus===p.id?'focused':''} ${classes}" data-panel-id="${p.id}" data-kind="${p.kind}" tabindex="-1"><header class="panel-header"><span class="panel-title">${esc(title)}</span><button class="panel-close" data-action="close" aria-label="Close ${esc(title)}">×</button></header>${body}${bar?`<footer class="panel-bar">${bar}</footer>`:''}</section>`;
}
function render(){
  const scrolls=new Map($$('[data-scroll]').map(el=>[el.dataset.scroll,{top:el.scrollTop,left:el.scrollLeft}]));
  const x=$('#stage').scrollLeft;
  $('#stage').innerHTML=ui.panels.map(renderPanel).join('')||'<div class="empty-canvas">Open a scene below.</div>';
  for(const el of $$('[data-scroll]')){const pos=scrolls.get(el.dataset.scroll);if(pos){el.scrollTop=pos.top;el.scrollLeft=pos.left;}}
  for(const p of ui.panels)if(p.values)for(const [name,value] of Object.entries(p.values)){const input=$(`[data-panel-id="${p.id}"] [name="${name}"]`);if(input)input.value=value;}
  $('#stage').scrollLeft=x;
  $$('.draft-controls [data-scene]').forEach(b=>b.setAttribute('aria-current',b.dataset.scene===ui.scene?'page':'false'));
}
function scene(name){
  ui.scene=name;ui.panels=[];
  const add=(kind,arg,parent,extra={})=>{const p=node(kind,arg,{parent:parent?.id||null,...extra});ui.panels.push(p);return p;};
  if(name==='projects'){
    const projects=add('projects',null,null,{cursor:'superapp'});
    const list=add('workspaces',null,projects,{filter:'@project:superapp',cursor:'zurich'});
    add('workspace','zurich',list);
    ui.focus=projects.id;
  }else if(name==='workspaces'){
    const list=add('workspaces',null,null,{cursor:'zurich'});add('workspace','zurich',list);ui.focus=list.id;
  }else if(name==='workspace'){
    const w=add('workspace','zurich');add('chat','implementation',w);ui.focus=w.id;
  }else if(name==='review'){
    const list=add('review','zurich',null,{cursor:'anchors'});add('diff','anchors',list);ui.focus=list.id;
  }else if(name==='steps'){
    const chat=add('chat','implementation');add('step','implementation',chat);ui.focus=chat.id;
  }else{const c=add('connections');ui.focus=c.id;}
  render();$('#stage').scrollLeft=0;
}
function table(p,head,right,rows){
  const error=filterError(p.filter,p.kind);
  const choices=p.suggestOpen?suggestions(p):[];
  return `<div class="panel-body table-body"><div class="filter-wrap"><input class="filter" data-filter="${p.id}" aria-label="Filter ${p.kind}" value="${esc(p.filter)}" placeholder="filter…  ( / )   @ for tags" autocomplete="off" spellcheck="false">${choices.length?`<div class="suggestions" role="listbox" aria-label="Filter tags">${choices.map((value,i)=>`<button class="suggestion ${i===(p.suggestIndex||0)?'active':''}" role="option" aria-selected="${i===(p.suggestIndex||0)}" data-suggestion="${value}">${value}</button>`).join('')}</div>`:''}</div>${error&&!choices.length?`<div class="filter-error">${esc(error)}</div>`:''}<div class="table-head"><span>${head}</span><span>${right}</span></div><div class="rows" data-scroll="${p.id}-rows">${rows||'<div class="table-empty">nothing under this filter</div>'}</div></div>`;
}
function suggestions(p){const token=p.filter.split(/\s+/).at(-1);if(!token.startsWith('@'))return [];const options=p.kind==='workspaces'?['@project:superapp','@project:reader','@project:dotfiles','@unread']:p.kind==='review'?['@unreviewed','@changed','@file:anchors.rs','@file:progress.rs','@file:workspace.rs','@file:review.rs']:[];return options.filter(v=>v.startsWith(token));}
function completeSuggestion(p,value){p.filter=p.filter.replace(/\S*$/,value+' ');p.suggestOpen=false;p.cursor=null;render();const input=$(`[data-filter="${p.id}"]`);input.focus();input.setSelectionRange(input.value.length,input.value.length);}
function filterError(filter,kind){const allowed=kind==='workspaces'?['project','unread']:kind==='review'?['file','unreviewed','changed']:[];const invalid=filter.split(/\s+/).find(w=>w.startsWith('@')&&!allowed.includes(w.slice(1).split(':')[0]));return invalid?`unknown tag: ${invalid}`:'';}
function termsMatch(filter,text,tags={}){
  return filter.toLowerCase().trim().split(/\s+/).filter(Boolean).every(t=>{
    if(!t.startsWith('@'))return text.toLowerCase().includes(t);
    const [name,...value]=t.slice(1).split(':');
    if(!(name in tags))return true;
    return value.length?String(tags[name]).toLowerCase().includes(value.join(':')):Boolean(tags[name]);
  });
}
function row(p,key,main,sub='',right='',extra=''){
  return `<button class="row ${p.cursor===key?'selected':''} ${p.marks.includes(key)?'marked':''} ${extra}" data-row="${esc(key)}"><span class="row-line"><span class="row-main">${esc(main)}</span>${right?`<span class="row-right">${esc(right)}</span>`:''}</span>${sub?`<span class="row-line"><span class="row-sub">${esc(sub)}</span></span>`:''}</button>`;
}
function projectsPanel(p){
  const projects=['superapp','reader','dotfiles'].filter(n=>termsMatch(p.filter,n));
  return panel(p,'projects',table(p,'project','workspaces',projects.map(id=>row(p,id,id,'',String(allWorkspaces().filter(w=>w.project===id).length))).join('')),link('add project','new-project')+link('workspaces','all-workspaces'),3);
}
function workspaceRows(p){
  const sorted=allWorkspaces().sort((a,b)=>a.minutes-b.minutes);
  return sorted.filter(w=>w.id===p.cursor||termsMatch(p.filter,`${w.id} ${w.project}`,{project:w.project,unread:workspaceUnread(w.id)}));
}
function workspacesPanel(p){
  const rows=workspaceRows(p);
  const html=rows.map(w=>row(p,w.id,w.id,`${p.filter.includes('@project:')?'':w.project}${w.status==='Running'?' · running':w.status==='Checks failed'?' · check failed':''}`,w.time.replace('just now','now').replace(' min ago','m').replace(' hours ago','h').replace(' hour ago','h'),workspaceUnread(w.id)?'unread':'')).join('');
  return panel(p,'workspaces',table(p,'workspace','activity',html),button('new workspace','new-workspace'));
}
function workspacePanel(p){
  const w=workspace(p.arg),isMain=w.id==='zurich';
  const hasPr=isMain?data.hasPr:w.pr!=='No PR';
  const chats=data.chats.filter(c=>c.workspace===w.id);
  const status=isMain?data.conflict?'rebase conflict':!hasPr?'':data.checks==='failed'?'1 check failed':data.prDraft?'draft':data.merged?'merged':'checks passed':w.status.toLowerCase();
  const prControl=hasPr?link(isMain?'#148':w.pr,'github'):button('create PR','create-pr');
  const body=`<div class="workspace-info"><div class="git-line"><span class="branch" title="${esc(w.branch)}">${esc(w.branch)}</span></div><div class="git-line"><span class="secondary">← origin/main</span><span class="spacer"></span>${prControl}<span class="small ${status.includes('failed')||status.includes('conflict')?'error':'secondary'}">${status}</span></div>${isMain&&data.snapshot!==data.publishedSnapshot?`<div class="git-line small"><span>changes not pushed</span><span class="spacer"></span>${button('push','push')}</div>`:''}</div><div class="workspace-chats"><div class="table-head"><span>chat</span><span>agent</span></div><div class="rows" data-scroll="${p.id}-chats">${chats.map(chat=>`<button class="row ${chat.unread?'unread':''}" data-chat="${chat.id}"><span class="row-line"><span class="row-main">${chatLabel(chat)}</span><span class="row-right">${chat.harness==='Claude Code'?'Claude':chat.harness}</span></span><span class="row-line"><span class="row-sub">${esc(chat.status==='Running'?'working…':chatPreview(chat))}</span></span></button>`).join('')||'<div class="table-empty">no chats yet</div>'}</div></div>${miniTerminal(p)}`;
  let bar=link('review','review')+button('new chat','new-chat')+link('activity','activity');
  if(isMain){bar+=button('ai review','ai-review');if(data.checks==='failed'||data.conflict)bar+=button(data.conflict?'fix conflicts':'fix errors','fix-errors');bar+=button(data.autoMerge?'auto-merge on':'auto-merge','auto-merge',!data.hasPr||data.prDraft||data.merged?'disabled':'')+button(data.merged?'merged':'merge','merge',!canMerge()?'disabled':'');}
  return panel(p,`${w.project} / ${w.id}`,body,bar);
}
function miniTerminal(p){
  const terminal=embeddedTerminal(p.arg);
  return `<div class="terminal-mini"><div class="terminal-heading"><span>terminal</span>${link('open panel','open-terminal')}</div><pre class="terminal-output" data-scroll="${p.id}-${terminal.id}">${esc(terminal.output.slice(-3).join('\n'))}</pre>${terminalForm(terminal)}</div>`;
}
function terminalForm(terminal){return `<form class="terminal-form" data-terminal="${terminal.id}"><span>❯</span><input aria-label="Terminal command" value="${esc(terminal.draft)}" placeholder="command…" autocomplete="off" spellcheck="false"></form>`;}
function terminalPanel(p){const terminal=data.terminals[p.arg];return panel(p,`terminal: ${terminal.workspace} / ${terminal.id.slice(1)}`,`<div class="panel-body terminal-body"><pre class="terminal-output" data-scroll="${p.id}-terminal">${esc(terminal.output.join('\n'))}</pre>${terminalForm(terminal)}</div>`,'',6);}

function reviewRows(p){return data.files.filter(file=>termsMatch(p.filter,file.path,{file:file.path,unreviewed:!file.reviewed,changed:file.changedSinceReview&&!file.reviewed}));}
function reviewPanel(p){
  if(p.arg!=='zurich')return panel(p,`review: ${workspace(p.arg).id}`,'<div class="panel-body form-body secondary">No changes yet.</div>','',3);
  const c=counts(),rows=reviewRows(p),current=fileRecord(p.cursor);
  if(current&&!rows.some(f=>f.id===p.cursor))rows.push(current);
  const meter=`<div class="review-meter"><div class="review-count"><span>${c.reviewed} / ${c.total} lines reviewed</span><span class="secondary">${c.total-c.reviewed} left</span></div><div class="progress" role="progressbar" aria-label="Reviewed lines" aria-valuemin="0" aria-valuemax="${c.total}" aria-valuenow="${c.reviewed}"><span class="progress-done" style="width:${c.reviewed/c.total*100}%"></span><span class="progress-changed" style="width:${c.changed/c.total*100}%"></span></div>${c.changed?`<div class="review-detail">${c.changed} to recheck · ${c.total-c.reviewed-c.changed} new</div>`:''}</div>`;
  const drawRow=file=>row(p,file.id,file.path.split('/').at(-1),'',`${file.reviewed?'✓ ':''}${lines([file]).length}`,file.reviewed?'':'unread');
  const hidden=p.marks.filter(id=>!rows.some(f=>f.id===id)).map(fileRecord).filter(Boolean);
  const html=(hidden.length?'<div class="hidden-marks">marked · hidden by the filter</div>'+hidden.map(f=>drawRow(f).replace('data-row=','data-hidden-row=')).join(''):'')+rows.map(drawRow).join('');
  const notice=ui.pendingEdit?`<div class="review-notice">new changes available · ${link('load changes','load-edit')}</div>`:data.reviewNotice?`<div class="review-notice">${esc(data.reviewNotice)}</div>`:'';
  const bar=p.marks.length?button(`mark ${p.marks.length} files reviewed`,'mark')+button('mark unreviewed','unmark'):'';
  return panel(p,'review: zurich',meter+notice+table(p,'file','lines',html),bar,3,'review-list');
}
function codeView(h,path){
  let old=h.oldStart,next=h.newStart;
  const number=(n,side)=>n===''?'<span class="number"></span>':`<button class="number" data-copy-reference="${esc(path)}:${n}${side==='base'?' (base)':''}" title="Copy ${esc(path)}:${n}${side==='base'?' (base)':''}" aria-label="Copy ${side} file and line ${n}">${n}</button>`;
  return `<div class="code-lines">${h.lines.map(l=>{const a=l.kind==='add'?'':old++,b=l.kind==='del'?'':next++;return `<div class="code-line ${l.kind}">${number(a,'base')}${number(b,'current')}<span class="sign">${l.kind==='add'?'+':l.kind==='del'?'−':''}</span><code>${esc(l.text)}</code></div>`;}).join('')}</div>`;
}
function diffPanel(p){
  const file=fileRecord(p.arg);if(!file)return panel(p,'diff','<div class="panel-body form-body">File no longer exists in this comparison.</div>','',6);
  const body=`<div class="diff-meta"><div>${file.path}</div><div>origin/main → working tree · ${lines([file]).length} changed lines · ${file.reviewed?'reviewed':'unreviewed'}</div></div><div class="panel-body diff-scroll" data-scroll="${p.id}-diff">${file.hunks.map(h=>`<div class="hunk-location">@@ −${h.oldStart} +${h.newStart} @@</div>${codeView(h,file.path)}`).join('')}</div>`;
  return panel(p,file.path.split('/').at(-1),body,button(file.reviewed?'mark unreviewed':'mark reviewed','toggle-file-review')+link('comment on GitHub','comment'),6);
}

function stepRecord(id){const original=fixtures();if(id==='edit'&&data.editStep)return data.editStep;return id==='tracking'?{before:2,after:3,path:original[1].path,hunk:original[1].hunks[0]}:{before:3,after:4,path:original[0].path,hunk:original[0].hunks[1]};}
function stepPanel(p){
  if(p.arg==='review')return panel(p,'Claude: review result',`<div class="panel-body form-body"><p class="prose">Add a test for a conflict resolved during rebase. The resolved file should need review again.</p><p class="form-note">Reviewed snapshot 4. Your review marks are unchanged.</p></div>`,link('post to GitHub','publish-suggestion'),4);
  const s=stepRecord(p.arg),added=s.hunk.lines.filter(l=>l.kind==='add').length,deleted=s.hunk.lines.filter(l=>l.kind==='del').length;
  return panel(p,`step: ${s.path.split('/').at(-1)}`,`<div class="diff-meta"><div>${s.path}</div><div>step ${s.before} → ${s.after} · +${added} −${deleted} · shared checkout</div></div><div class="panel-body diff-scroll" data-scroll="${p.id}-diff">${codeView(s.hunk,s.path)}</div>`,link('current change','current-change'),6);
}
function stepChangeCount(id){
  if(id==='review')return '1 suggestion';
  const h=stepRecord(id).hunk;return `+${h.lines.filter(l=>l.kind==='add').length} −${h.lines.filter(l=>l.kind==='del').length}`;
}
function chatPanel(p){
  const chat=data.chats.find(c=>c.id===p.arg);if(!chat)return panel(p,'chat','<div class="panel-body form-body">Chat not found.</div>');
  const messages=chat.messages.map(m=>`<article class="message ${m.role==='You'?'user':''}"><div class="message-meta"><span>${esc(m.role)}</span></div><p class="prose">${esc(m.text)}</p>${m.step?`<button class="step-link" data-step="${m.step}"><span class="row-main">${m.step==='review'?'review result':'view changes'}</span><span class="small secondary">${stepChangeCount(m.step)}</span><span>→</span></button>`:''}</article>`).join('');
  const model=chat.model==='Configured default'?'default':chat.model;
  const started=chat.messages.some(m=>m.role==='You');
  const body=`<div class="chat-settings"><select data-provider="${chat.id}" aria-label="Provider${started?' (opens in new chat)':''}" title="${started?'Changing provider opens a new chat':'Provider'}" ${chat.status==='Running'?'disabled':''}>${['Codex','Claude Code'].map(v=>`<option ${v===chat.harness?'selected':''}>${v}</option>`).join('')}</select><select data-model="${chat.id}" aria-label="Model" ${chat.status==='Running'?'disabled':''}>${[model,...(chat.harness==='Codex'?['default','fast']:['Opus','Sonnet'])].filter((v,i,a)=>a.indexOf(v)===i).map(v=>`<option ${v===model?'selected':''}>${esc(v)}</option>`).join('')}</select><span class="spacer"></span><span class="secondary">${chat.status.toLowerCase()}</span></div><div class="panel-body chat-body" data-scroll="${p.id}-chat">${messages}</div><form class="chat-compose"><textarea data-chat-draft="${chat.id}" aria-label="Message" placeholder="message…">${esc(chat.draft||'')}</textarea><div class="compose-tools"><select aria-label="Agent mode"><option>work</option><option>plan</option></select><span class="spacer"></span>${chat.status==='Running'?button('stop','stop-agent'):'<button class="action" type="submit">send</button>'}</div></form>`;
  return panel(p,`${chatLabel(chat)}: ${chat.harness}`,body,button('new chat','new-chat'));
}

function activityPanel(p){
  if(p.arg!=='zurich')return panel(p,`activity: ${p.arg}`,'<div class="panel-body form-body secondary">No steps yet.</div>');
  const events=[...data.activity,{title:'Claude finished review',description:'1 suggestion',step:'review'},{title:'Codex: ambiguous matches',description:'+9 −1',step:'implementation'},{title:'Codex: line progress',description:'+13 −2',step:'tracking'}];
  return panel(p,'activity: zurich',`<div class="panel-body table-body"><div class="table-head"><span>step</span><span>changes</span></div><div class="rows">${events.map(e=>`<button class="row" data-step="${e.step||'implementation'}"><span class="row-line"><span class="row-main">${esc(e.title)}</span></span><span class="row-sub">${esc(e.description)}</span></button>`).join('')}</div></div>`);
}
function checksPanel(p){
  const body=`<div class="panel-body form-body"><p class="form-note">PR #148 · published snapshot ${data.publishedSnapshot}${data.snapshot!==data.publishedSnapshot?`<br>Local snapshot ${data.snapshot} is not checked.`:''}</p>${['build','review tests','format'].map(n=>`<div class="check-row"><span>${n}</span><span class="${n==='review tests'&&data.checks==='failed'?'error':'secondary'}">${n==='review tests'&&data.checks==='failed'?'failed':'passed'}</span></div>`).join('')}${data.checks==='failed'?'<pre class="log error">conflict_resolution_requires_review\nexpected: NeedsReview\nactual: Reviewed\n\n1 failed; 11 passed</pre>':''}${data.conflict?'<p class="form-note error">Rebase conflict in anchors.rs.</p>':''}</div>`;
  return panel(p,'checks: #148',body,(data.checks==='failed'||data.conflict?button(data.conflict?'fix conflicts':'fix errors','fix-errors'):'')+button('rerun checks','rerun'));
}
function hasPr(workspaceId){return workspaceId==='zurich'?data.hasPr:workspace(workspaceId).pr!=='No PR';}
function prLabel(workspaceId){return workspaceId==='zurich'?'#148':workspace(workspaceId).pr;}
function githubPanel(p){
  const w=workspace(p.arg),main=w.id==='zurich',exists=hasPr(w.id),draft=main?data.prDraft:w.prDraft;
  const state=!exists?'no pull request':main&&data.merged?'merged':draft?'draft':'open';
  const comments=data.comments.filter(c=>(c.workspace||'zurich')===w.id);
  return panel(p,exists?`pull request: ${prLabel(w.id)}`:'pull request',`<div class="panel-body form-body"><dl class="field"><dt>branch</dt><dd>${esc(w.branch)} → main</dd></dl><dl class="field"><dt>state</dt><dd>${state}</dd></dl>${exists&&main?`<dl class="field"><dt>checks</dt><dd>${data.checks==='passing'?'3 passed':'1 failed'}</dd></dl><dl class="field"><dt>auto-merge</dt><dd>${data.autoMerge?'on':'off'}</dd></dl>`:''}${comments.map(c=>`<article class="comment"><div class="small">you · GitHub · ${esc(c.target)}</div><p class="prose">${esc(c.text)}</p></article>`).join('')}</div>`,!exists?button('create PR','create-pr'):link('comment on GitHub','comment')+(draft?button('ready for review','ready-pr'):'')+(main?link('checks','checks'):''));
}
function connectionsPanel(p){return panel(p,'connections',`<div class="panel-body form-body">${[['Codex','ChatGPT subscription'],['Claude Code','Claude subscription'],['GitHub','signed in']].map(([name,description])=>`<div class="check-row"><span>${name}<br><span class="small secondary">${description}</span></span>${link('sign in','sign-in',`data-provider="${name}"`)}</div>`).join('')}</div>`,link('agent tools','tools'));
}
function formPanel(p){return panel(p,p.arg.title,`<div class="panel-body form-body">${p.arg.body}</div>`,p.arg.bar||button('done','close'),p.arg.width||4);}
function renderPanel(p){return ({projects:projectsPanel,workspaces:workspacesPanel,workspace:workspacePanel,review:reviewPanel,diff:diffPanel,step:stepPanel,chat:chatPanel,terminal:terminalPanel,activity:activityPanel,checks:checksPanel,github:githubPanel,connections:connectionsPanel,form:formPanel}[p.kind]||formPanel)(p);}

function contextWorkspace(p){
  if(!p)return 'zurich';
  if(['workspace','review','github','checks','activity'].includes(p.kind))return p.arg||'zurich';
  if(p.kind==='chat')return data.chats.find(c=>c.id===p.arg)?.workspace||'zurich';
  if(p.kind==='terminal')return data.terminals[p.arg]?.workspace||'zurich';
  if(p.kind==='form'&&p.arg.workspace)return p.arg.workspace;
  return p.parent?contextWorkspace(nodeById(p.parent)):'zurich';
}
function showForm(parent,title,body,bar,extra={}){return openPanel(parent?.id,'form',{title,body,bar,...extra});}
function field(label,name,html){return `<div class="field"><label for="${name}">${label}</label>${html}</div>`;}
function formValue(p,name){return p.values?.[name]??$(`[data-panel-id="${p.id}"] [name="${name}"]`)?.value??'';}
function selectRow(p,key,independent=false){
  p.cursor=key;
  if(p.kind==='projects')return openPanel(p.id,'workspaces',null,{filter:`@project:${key}`,keepFocus:true,independent});
  if(p.kind==='workspaces')return openPanel(p.id,'workspace',key,{keepFocus:true,independent});
  if(p.kind==='review')return openPanel(p.id,'diff',key,{keepFocus:true,independent});
}
function touchChat(chat){chat.lastUsed=Date.now();data.recentChats[chat.workspace]=chat.id;}
function openChat(parent,id,independent=false){
  const chat=data.chats.find(c=>c.id===id);if(!chat)return;
  chat.unread=false;touchChat(chat);save();
  return openPanel(parent?.id,'chat',id,{independent});
}
function chatParent(p){return p?.kind==='chat'?nodeById(p.parent):p;}
function openReview(parent){const id=contextWorkspace(parent);const p=openPanel(parent.id,'review',id,{cursor:id==='zurich'?'anchors':null});if(id==='zurich')openPanel(p.id,'diff','anchors',{keepFocus:true});}
function mark(p,reviewed){
  const ids=p.kind==='diff'?[p.arg]:p.kind==='review'?(p.marks.length?p.marks:[p.cursor]):[];
  const targets=ids.map(fileRecord).filter(Boolean);if(!targets.length)return;
  targets.forEach(file=>{file.reviewed=reviewed;if(reviewed)file.changedSinceReview=false;});
  if(p.kind==='diff'){const list=nodeById(p.parent);if(list?.kind==='review')list.cursor=p.arg;}
  p.marks=[];save();render();
}
function createWorkspace(p){
  const tagged=(p.filter.match(/@project:(\S+)/)||[])[1];
  const project=['superapp','reader','dotfiles'].includes(tagged)?tagged:workspace(p.cursor||allWorkspaces()[0].id).project;
  const cities=['basel','lucerne','lausanne','geneva','lugano','sion','thun','aarau'];
  let label=cities.find(city=>!allWorkspaces().some(w=>w.id===city));
  if(!label){let n=2;while(allWorkspaces().some(w=>w.id===`basel-v${n}`))n++;label=`basel-v${n}`;}
  const w={id:label,project,branch:`work/${label}`,pr:'No PR',status:'Idle',time:'now',minutes:-Date.now(),created:true};
  data.createdWorkspaces.unshift(w);p.cursor=w.id;
  const chat=freshChat(w.id);embeddedTerminal(w.id);save();
  const opened=openPanel(p.id,'workspace',w.id);openChat(opened,chat.id);
}
function resolveChat(p){
  const id=contextWorkspace(p);
  let ancestor=p,workspacePanel=null;
  while(ancestor){
    if(ancestor.kind==='chat'){const chat=data.chats.find(c=>c.id===ancestor.arg);if(chat?.workspace===id)return chat;}
    if(ancestor.kind==='workspace'&&ancestor.arg===id){workspacePanel=ancestor;break;}
    ancestor=nodeById(ancestor.parent);
  }
  if(workspacePanel){let child=ui.panels.find(n=>n.parent===workspacePanel.id);while(child){
    if(child.kind==='chat'){const chat=data.chats.find(c=>c.id===child.arg);if(chat?.workspace===id)return chat;}
    child=ui.panels.find(n=>n.parent===child.id);
  }}
  return data.chats.find(c=>c.id===data.recentChats[id]&&c.workspace===id)||data.chats.filter(c=>c.workspace===id).sort((a,b)=>(b.lastUsed||0)-(a.lastUsed||0))[0]||freshChat(id);
}
function revealChat(p,chat){
  const existing=ui.panels.find(n=>n.kind==='chat'&&n.arg===chat.id);
  if(existing){chat.unread=false;touchChat(chat);save();ui.focus=existing.id;render();ensureVisible(existing.id);return existing;}
  const w=ui.panels.find(n=>n.kind==='workspace'&&n.arg===chat.workspace);
  return openChat(w||chatParent(p),chat.id);
}
function requestPr(p,draft=false){
  const id=contextWorkspace(p),w=workspace(id);
  if(data.pendingPr[id]){
    const pending=data.pendingPr[id],chat=data.chats.find(c=>c.id===pending.chatId);
    if(draft&&!pending.draft){pending.draft=true;chat.messages.push({role:'You',text:'Make this a draft PR so I can post a comment before it is ready for review.'});save();}
    return revealChat(p,chat);
  }
  const chat=resolveChat(p);
  chat.messages.push({role:'You',text:`Create ${draft?'a draft pull request':'a pull request'} for ${w.branch} against origin/main. Review the changes, run relevant checks, commit and push as needed, and create the PR on GitHub.${draft?' I have a comment to post once the draft PR exists.':''}`});
  chat.status='Running';data.pendingPr[id]={chatId:chat.id,draft};save();revealChat(p,chat);
}
function completePr(){
  const pending=Object.entries(data.pendingPr);if(!pending.length)return toast('Use create PR to send a request first.');
  for(const [id,request] of pending){
    if(id==='zurich'){data.hasPr=true;data.prDraft=request.draft;data.publishedSnapshot=data.snapshot;}
    else{const w=workspace(id);w.pr=`#${149+allWorkspaces().indexOf(w)}`;w.prDraft=request.draft;}
    const chat=data.chats.find(c=>c.id===request.chatId);chat.status='Ready';chat.unread=true;
    chat.messages.push({role:chat.harness,text:`Created ${request.draft?'draft ':''}PR ${prLabel(id)}.${data.commentDrafts[id]?' Your comment is ready to post from the PR panel.':''}`});
    delete data.pendingPr[id];
  }
  save();render();toast('Sample PR created.');
}
function applyEdit(){
  if(data.editApplied)return toast('The sample edit has already been applied.');
  const file=data.files[0],h=file.hunks[0];
  const replacements={'a1-3':'    let candidates = old.index.lookup_exact(new.content_hash);','a1-4':'    let matched = candidates.iter().filter(|candidate| {','a1-5':'        candidate.same_side_and_origin(new)','a1-6':'            && candidate.bytes() == new.bytes()','a1-7':'            && candidate.context_is_unchanged(new)'};
  const additions=['    if new.has_conflict_resolution() {','        marks.require_review(new.id);','    }'];
  const delta=h.lines.filter(l=>l.kind!=='del').flatMap(l=>replacements[l.id]?['-'+l.text,'+'+replacements[l.id]]:[' '+l.text]);
  delta.splice(delta.length-1,0,...additions.map(t=>'+'+t));
  data.editStep={before:data.snapshot,after:data.snapshot+1,path:file.path,hunk:makeHunk('edit-step','Context verification',h.newStart,h.newStart,delta.join('\n'))};
  for(const line of h.lines)if(replacements[line.id])line.text=replacements[line.id];
  h.lines.splice(h.lines.length-1,0,...additions.map((text,i)=>({id:`edit-${i}`,kind:'add',text})));
  file.changedSinceReview ||= file.reviewed;file.reviewed=false;
  data.editApplied=true;data.snapshot++;ui.pendingEdit=false;
  data.reviewNotice='anchors.rs changed · review the file again';
  data.activity.unshift({title:'Codex: context verification',description:'+8 −5',step:'edit'});
  const chat=data.chats.find(c=>c.id==='implementation');chat.unread=true;chat.messages.push({role:'Codex',text:'Added exact context matching and a check for conflict resolutions.',step:'edit'});
  save();render();
}
function simulate(name){
  if(name==='edit'){if(data.editApplied)return toast('The sample edit has already been applied.');if(ui.panels.some(p=>p.kind==='diff')){ui.pendingEdit=true;render();}else applyEdit();return;}
  if(name==='pr-created'){completePr();return;}
  if(name==='rebase'){
    if(data.rebased)return toast('The sample rebase is already applied.');
    data.files.forEach(f=>f.hunks.forEach(h=>{h.oldStart+=17;h.newStart+=17;}));
    data.files[1].path='src/review/line_progress.rs';data.rebased=true;data.snapshot++;
    data.reviewNotice=`${counts().reviewed} reviewed lines kept through rebase`;
  }
  if(name==='failure')data.checks='failed';
  if(name==='conflict')data.conflict=true;
  if(name==='resolved'){
    const file=data.files[0];file.changedSinceReview ||= file.reviewed;file.reviewed=false;
    data.conflict=false;data.snapshot++;data.reviewNotice='anchors.rs needs review after conflict resolution';
  }
  if(name==='no-pr'){data.hasPr=false;data.prDraft=false;data.merged=false;data.autoMerge=false;delete data.pendingPr.zurich;}
  if(name==='offline')data.offline=true;
  if(name==='online')data.offline=false;
  save();render();
}
function showComment(parent,text=''){
  const id=contextWorkspace(parent),saved=data.commentDrafts[id];
  const file=parent.kind==='diff'?fileRecord(parent.arg):null;
  const path=file?.path||saved?.path;
  const published=id!=='zurich'||data.snapshot===data.publishedSnapshot;
  const target=path&&published?`${path} · file comment`:'general PR comment';
  const draftText=text||saved?.text||'';
  const note=hasPr(id)?!published?'Push these changes to comment on the file. This will be a general PR comment.':'':'Create a draft pull request to post this comment on GitHub.';
  showForm(parent,hasPr(id)?`comment: ${prLabel(id)}`:'comment on GitHub',`<p class="form-note">${esc(target)}</p><textarea name="comment" aria-label="GitHub comment" style="width:100%;min-height:150px" placeholder="comment…">${esc(draftText)}</textarea>${note?`<p class="form-note">${note}</p>`:''}<p class="form-note error" data-send-status></p>`,hasPr(id)?button('post to GitHub','post-comment')+button('cancel','close'):button('create draft PR','create-pr-comment')+button('cancel','close'),{target,path,workspace:id,source:parent.id});
}

function canMerge(){return data.hasPr&&!data.prDraft&&!data.merged&&!data.conflict&&data.checks==='passing'&&data.snapshot===data.publishedSnapshot;}
const actions={
  close:p=>closePanel(p.id),
  reset(){resetting=true;localStorage.removeItem(STORE);location.reload();},
  'all-workspaces':p=>openPanel(p.id,'workspaces',null),
  review:openReview,
  activity:p=>openPanel(p.id,'activity',contextWorkspace(p)),
  github:p=>openPanel(p.id,'github',contextWorkspace(p)),
  checks:p=>openPanel(p.id,'checks',contextWorkspace(p)),
  mark:p=>mark(p,true),unmark:p=>mark(p,false),
  'toggle-file-review':p=>mark(p,!fileRecord(p.arg).reviewed),
  'load-edit':applyEdit,
  'open-terminal'(p){const id=contextWorkspace(p),terminal=embeddedTerminal(id);data.embeddedTerminals[id]=freshTerminal(id);save();openPanel(p.id,'terminal',terminal.id,{independent:true});},
  'new-project'(p){showForm(p,'add project',field('folder','project-path','<input name="project-path" id="project-path" placeholder="~/code/project">'),button('add','add-project'));},
  'add-project'(p){if(!formValue(p,'project-path').trim())return;toast('Folder selection previewed; no repository was added.');closePanel(p.id);},
  'new-workspace':createWorkspace,
  'new-chat'(p){const chat=freshChat(contextWorkspace(p));openChat(chatParent(p),chat.id);},
  'stop-agent'(p){const chat=data.chats.find(c=>c.id===p.arg);chat.status='Stopped';for(const [id,req] of Object.entries(data.pendingPr))if(req.chatId===chat.id)delete data.pendingPr[id];save();render();},
  'ai-review'(p){const id=contextWorkspace(p);const chat=data.chats.find(c=>c.id==='reviewer'&&c.workspace===id)||freshChat(id);chat.messages.push({role:'You',text:`Review snapshot ${data.snapshot}.`},{role:chat.harness,text:'Sample review complete. Add coverage for conflict resolutions. Your personal file marks are unchanged.',step:'review'});openChat(p,chat.id);},
  'fix-errors'(p){const chat=resolveChat(p);chat.status='Running';chat.messages.push({role:'You',text:data.conflict?'Resolve the rebase conflict in anchors.rs.':'Fix the failed conflict_resolution_requires_review check.'});revealChat(p,chat);},
  rerun(){data.checks='passing';data.chats.forEach(c=>{if(c.status==='Running'&&!Object.values(data.pendingPr).some(r=>r.chatId===c.id))c.status='Ready';});save();render();toast('Sample check rerun passed.');},
  'current-change'(p){const file=p.arg==='tracking'?'progress':'anchors';const list=openPanel(p.id,'review','zurich',{cursor:file});openPanel(list.id,'diff',file,{keepFocus:true});},
  comment(p){showComment(p);},
  'publish-suggestion'(p){showComment(p,'Add a regression test for conflict resolutions during rebase. Resolved files should require review again.');},
  'create-pr-comment'(p){data.commentDrafts[p.arg.workspace]={text:formValue(p,'comment'),path:p.arg.path};requestPr(p,true);},
  'post-comment'(p){const text=formValue(p,'comment').trim();if(!text)return;const id=contextWorkspace(p);if(!hasPr(id))return showComment(p,text);if(data.offline){$(`[data-panel-id="${p.id}"] [data-send-status]`).textContent='Not sent · GitHub is offline. Your draft is kept here.';return;}data.comments.push({text,target:p.arg.target,workspace:id});delete data.commentDrafts[id];save();const source=nodeById(p.arg.source);removeChain(p.id);openPanel(source?.id,'github',id);toast('Sample GitHub comment posted.');},
  'create-pr':p=>requestPr(p),
  'ready-pr'(p){const id=contextWorkspace(p);if(id==='zurich')data.prDraft=false;else workspace(id).prDraft=false;save();render();},
  push(){if(data.offline)return toast('Push failed · GitHub is offline.');data.publishedSnapshot=data.snapshot;data.checks='passing';save();render();toast('Sample push complete; checks passed.');},
  'auto-merge'(p){if(data.autoMerge){data.autoMerge=false;save();render();return;}showForm(p,'auto-merge: #148','<p class="prose">Squash when GitHub’s required checks and reviews pass.</p><p class="form-note">Your review progress does not block auto-merge. GitHub can merge later pushed revisions while this app is closed.</p>',button('enable auto-merge','confirm-auto'));},
  'confirm-auto'(p){data.autoMerge=true;save();closePanel(p.id);},
  merge(p){const c=counts();showForm(p,'merge: #148',`<p class="prose">review/line-anchors → main</p><p class="form-note">${c.reviewed}/${c.total} lines reviewed by you · ${c.total-c.reviewed} unreviewed<br>3 required checks passed</p>`+field('method','method','<select name="method" id="method"><option>squash</option><option>merge commit</option><option>rebase</option></select>'),button('merge pull request','confirm-merge'));},
  'confirm-merge'(p){if(!canMerge())return toast('PR state changed; check the current head before merging.');data.merged=true;data.autoMerge=false;save();closePanel(p.id);toast('Sample PR merged.');},
  'sign-in'(p,b){showForm(p,`${b.dataset.provider}: sign in`,'<p class="prose">Continue in the provider’s sign-in flow, then return here.</p><p class="form-note">Sign-in is a preview. No credentials are read.</p>',button('done','close'));},
  tools(p){showForm(p,'agent tools','<pre class="log">workshop.workspaces.list / create\nworkshop.chats.start / send / stop\nworkshop.chats.set_provider / set_model\nworkshop.steps.diff\nworkshop.review.progress / mark_file\nworkshop.github.create_pr / comment / merge\nworkshop.git.push\nworkshop.terminal.open_panel</pre><p class="form-note">Proposed tools over the same actions and local SQLite state. File review writes carry the actor and displayed snapshot.</p>',button('done','close'));}
};


async function copyReference(text){
  try{
    if(navigator.clipboard?.writeText)await navigator.clipboard.writeText(text);
    else{const input=document.createElement('textarea');input.value=text;input.style.cssText='position:fixed;opacity:0';document.body.append(input);input.select();const copied=document.execCommand('copy');input.remove();if(!copied)throw new Error('Copy failed');}
    toast(`Copied ${text}`);
  }catch{toast('Could not copy the file reference.');}
}

// Ordinary panel navigation: click previews, Cmd-click keeps an independent panel.
document.addEventListener('click',event=>{
  const sceneButton=event.target.closest('[data-scene]');if(sceneButton){scene(sceneButton.dataset.scene);return;}
  const el=event.target.closest('[data-panel-id]'),p=el?nodeById(el.dataset.panelId):null;
  if(p){ui.focus=p.id;if(p.kind==='chat'){touchChat(data.chats.find(c=>c.id===p.arg));save();}syncFocus();}
  const action=event.target.closest('[data-action]');if(action&&!action.disabled){ui.nextIndependent=action.classList.contains('link')&&(event.metaKey||event.ctrlKey);try{actions[action.dataset.action]?.(p,action);}finally{ui.nextIndependent=false;}return;}
  if(!p)return;
  const suggestion=event.target.closest('[data-suggestion]');if(suggestion){completeSuggestion(p,suggestion.dataset.suggestion);return;}
  const r=event.target.closest('[data-row],[data-hidden-row]');if(r){selectRow(p,r.dataset.row||r.dataset.hiddenRow,event.metaKey||event.ctrlKey);return;}
  const chat=event.target.closest('[data-chat]');if(chat){openChat(p,chat.dataset.chat,event.metaKey||event.ctrlKey);return;}
  const step=event.target.closest('[data-step]');if(step){openPanel(p.id,'step',step.dataset.step,{independent:event.metaKey||event.ctrlKey});return;}
  const reference=event.target.closest('[data-copy-reference]');if(reference){
    copyReference(reference.dataset.copyReference);
  }
});
document.addEventListener('input',event=>{
  const el=event.target.closest('[data-panel-id]'),p=el?nodeById(el.dataset.panelId):null;if(!p)return;
  const target=event.target;
  if(target.matches('[data-filter]')){const pos=target.selectionStart;p.filter=target.value;p.cursor=null;p.suggestOpen=true;p.suggestIndex=0;render();const input=$(`[data-filter="${p.id}"]`);input.focus();input.setSelectionRange(pos,pos);}
  if(target.matches('.terminal-form input')){data.terminals[target.closest('[data-terminal]').dataset.terminal].draft=target.value;save();}
  if(target.matches('[data-chat-draft]')){const c=data.chats.find(c=>c.id===target.dataset.chatDraft);c.draft=target.value;touchChat(c);save();}
  if(p.kind==='form'&&target.name){p.values??={};p.values[target.name]=target.value;if(target.name==='comment'){data.commentDrafts[p.arg.workspace]={text:target.value,path:p.arg.path};save();}}
});
document.addEventListener('change',event=>{
  const t=event.target;if(t.id==='simulation'){simulate(t.value);t.value='';return;}
  const el=t.closest('[data-panel-id]'),p=el?nodeById(el.dataset.panelId):null;
  if(t.matches('[data-model]')){data.chats.find(c=>c.id===t.dataset.model).model=t.value;save();}
  if(t.matches('select[data-provider]')){
    const chat=data.chats.find(c=>c.id===t.dataset.provider),provider=t.value;
    if(provider===chat.harness||chat.status==='Running')return;
    if(chat.messages.some(m=>m.role==='You')){const next=freshChat(chat.workspace,provider,defaultModel(provider));openChat(chatParent(p),next.id);}
    else{chat.harness=provider;chat.model=defaultModel(provider);save();render();}
  }
  if(p?.kind==='form'&&t.name){p.values??={};p.values[t.name]=t.value;}
});

document.addEventListener('submit',event=>{
  event.preventDefault();const p=nodeById(event.target.closest('[data-panel-id]').dataset.panelId);
  if(event.target.matches('.terminal-form')){
    const terminal=data.terminals[event.target.dataset.terminal],cmd=terminal.draft.trim();if(!cmd)return;
    const w=workspace(terminal.workspace);
    terminal.output.push('❯ '+cmd,cmd==='pwd'?`~/workspaces/${w.project}/${w.id}`:cmd.startsWith('git status')?`${w.branch} · ${w.id==='zurich'?'4 changed files':'working tree clean'}`:cmd.startsWith('cargo test')?'12 passed; 0 failed':'Preview only; command was not executed.');
    terminal.draft='';save();render();$(`[data-panel-id="${p.id}"] .terminal-form input`)?.focus();
  }
  if(event.target.matches('.chat-compose')){const chat=data.chats.find(c=>c.id===p.arg),text=chat.draft.trim();if(!text)return;chat.messages.push({role:'You',text},{role:chat.harness,text:'Message saved in this sample chat. No agent process was started.'});chat.draft='';touchChat(chat);save();render();const body=$(`[data-panel-id="${p.id}"] .chat-body`);body.scrollTop=body.scrollHeight;}
});
document.addEventListener('keydown',event=>{
  const focused=event.target.closest('[data-panel-id]');const p=focused?nodeById(focused.dataset.panelId):nodeById(ui.focus);if(!p)return;
  if((event.metaKey||event.ctrlKey)&&event.key==='w'){event.preventDefault();closePanel(p.id);return;}
  const editing=event.target.matches('input,textarea,select');
  if(editing&&event.target.matches('[data-filter]')&&p.suggestOpen){const choices=suggestions(p);if(choices.length&&['ArrowDown','ArrowUp','Enter','Tab','Escape'].includes(event.key)){event.preventDefault();if(event.key==='Enter'||event.key==='Tab'){completeSuggestion(p,choices[p.suggestIndex||0]);return;}if(event.key==='Escape')p.suggestOpen=false;else p.suggestIndex=((p.suggestIndex||0)+(event.key==='ArrowDown'?1:-1)+choices.length)%choices.length;render();$(`[data-filter="${p.id}"]`).focus();return;}}
  if(editing){if(event.target.matches('[data-filter]')&&event.key==='ArrowDown'){event.preventDefault();const first=$(`[data-panel-id="${p.id}"] [data-row]`);first?.focus();if(first)selectRow(p,first.dataset.row);}return;}
  if(event.key==='Escape'){p.marks=[];render();return;}
  if(event.key==='/'||event.key==='Tab'){const f=$(`[data-filter="${p.id}"]`);if(f){event.preventDefault();f.focus();}return;}
  if(['ArrowDown','ArrowUp','Enter'].includes(event.key)&&['projects','workspaces','review'].includes(p.kind)){
    const rows=$$(`[data-panel-id="${p.id}"] [data-row]`),current=rows.findIndex(r=>r.dataset.row===p.cursor);
    const index=event.key==='Enter'?Math.max(current,0):Math.max(0,Math.min(rows.length-1,current+(event.key==='ArrowDown'?1:-1)));
    if(rows[index]){event.preventDefault();const preview=selectRow(p,rows[index].dataset.row,event.metaKey||event.ctrlKey);if(event.key==='Enter'&&preview){ui.focus=preview.id;syncFocus();$(`[data-panel-id="${preview.id}"]`)?.focus({preventScroll:true});}else $(`[data-panel-id="${p.id}"] [data-row="${rows[index].dataset.row}"]`)?.focus({preventScroll:true});}return;
  }
  if(event.key===' '&&p.kind==='review'&&p.cursor){event.preventDefault();p.marks=p.marks.includes(p.cursor)?p.marks.filter(id=>id!==p.cursor):[...p.marks,p.cursor];render();return;}
  if(event.key==='r'&&['diff','review'].includes(p.kind)){event.preventDefault();mark(p,true);}
});
window.addEventListener('beforeunload',()=>{if(!resetting)save();});
scene('workspaces');
