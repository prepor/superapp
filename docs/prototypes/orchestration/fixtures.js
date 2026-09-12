'use strict';

// Local fixtures for the UI draft; no repository or provider is contacted.
const makeHunk = (id, title, oldStart, newStart, code) => ({
  id, title, oldStart, newStart,
  lines: code.split('\n').map((raw, i) => ({id:`${id}-${i}`, kind:raw[0] === '+' ? 'add' : raw[0] === '-' ? 'del' : 'context', text:raw.slice(1)}))
});
function fixtures() {
  return [
    {id:'anchors',path:'src/review/anchors.rs',hunks:[
      makeHunk('a1','Match unchanged content',24,24,` pub fn carry_review(old: &Snapshot, new: &Snapshot) {
-    let key = (new.commit, new.path, new.line);
-    marks.copy_from(old.marks.get(&key));
+    let candidates = old.index.lookup(new.content_hash);
+    let matched = candidates.iter().find(|candidate| {
+        candidate.side == new.side
+            && candidate.content == new.content
+            && candidate.context_matches(new)
+    });
+    if let Some(previous) = matched {
+        marks.carry(previous.id, new.id);
+    } else {
+        marks.require_review(new.id);
+    }
 }`),
      makeHunk('a2','Ambiguous matches',48,57,` fn resolve_candidate(matches: &[Anchor]) -> Resolution {
-    Resolution::Matched(matches[0].clone())
+    match matches {
+        [only] if only.context_is_unchanged() => {
+            Resolution::Matched(only.clone())
+        }
+        [] => Resolution::New,
+        _ => Resolution::NeedsReview {
+            reason: ReviewReason::Ambiguous,
+        },
+    }
 }`)
    ]},
    {id:'progress',path:'src/review/progress.rs',hunks:[makeHunk('p1','Count changed lines',11,11,` pub fn progress(files: &[FileDiff]) -> Progress {
-    let total = files.len();
-    let reviewed = files.iter().filter(|f| f.reviewed).count();
+    let total = files.iter().map(FileDiff::changed_lines).sum();
+    let reviewed = files.iter()
+        .filter(|file| file.human_reviewed())
+        .map(FileDiff::changed_lines).sum();
+    let changed = files.iter()
+        .filter(|file| file.changed_since_review())
+        .map(FileDiff::changed_lines).sum();
+    Progress {
+        reviewed,
+        changed,
+        unseen: total - reviewed - changed,
+        total,
+    }
 }`)]},
    {id:'workspace',path:'src/workspace.rs',hunks:[makeHunk('w1','Pin displayed snapshot',82,82,` impl Workspace {
+    pub fn begin_review(&mut self) -> ReviewSession {
+        ReviewSession::new(self.snapshot.id)
+    }
+
+    pub fn accept_snapshot(&mut self, next: Snapshot) {
+        self.review.map_to(&next);
+        self.snapshot = next;
+    }
 }`)]},
    {id:'tests',path:'tests/review.rs',hunks:[
      makeHunk('t1','Keep marks after rebase',1,1,`+#[test]
+fn rebase_keeps_reviewed_content() {
+    let mut workspace = fixture();
+    workspace.review_all();
+    let before = workspace.reviewed_lines();
+    workspace.rebase_onto("main");
+    assert_eq!(workspace.reviewed_lines(), before);
+}`),
      makeHunk('t2','Recheck edited lines',10,10,`+#[test]
+fn changed_content_needs_review() {
+    let mut workspace = reviewed_fixture();
+    workspace.edit_line("anchors.rs", 28);
+    assert!(!workspace.file("anchors.rs").reviewed());
+}
+
+#[test]
+fn duplicate_content_is_not_auto_reviewed() {
+    let mut workspace = reviewed_fixture();
+    workspace.duplicate_block("anchors.rs");
+    assert!(!workspace.file("anchors.rs").reviewed());
+}`)
    ]}
  ];
}
const workspaces = [
  {id:'zurich',name:'Persistent code review',project:'superapp',branch:'review/line-anchors',status:'Needs review',agents:'2 chats',review:0,total:0,pr:'#148',time:'just now',minutes:0},
  {id:'oslo',name:'Calendar keyboard navigation',project:'superapp',branch:'calendar/keyboard',status:'Running',agents:'1 running',review:36,total:92,pr:'#146',time:'4 min ago',minutes:4},
  {id:'kyoto',name:'Faster feed refresh',project:'reader',branch:'perf/feed-refresh',status:'Checks failed',agents:'1 waiting',review:48,total:48,pr:'#32',time:'12 min ago',minutes:12},
  {id:'bern',name:'Terminal selection',project:'superapp',branch:'terminal/selection',status:'Ready to merge',agents:'2 chats',review:61,total:61,pr:'#145',time:'28 min ago',minutes:28},
  {id:'porto',name:'Simplify query cache',project:'superapp',branch:'refactor/query-cache',status:'Idle',agents:'1 chat',review:0,total:124,pr:'No PR',time:'1 hour ago',minutes:60},
  {id:'nara',name:'Document local setup',project:'dotfiles',branch:'docs/local-setup',status:'Idle',agents:'1 chat',review:18,total:24,pr:'#9',time:'2 hours ago',minutes:120},
  {id:'lima',name:'Remove stale subscriptions',project:'reader',branch:'fix/subscriptions',status:'Merged',agents:'1 chat',review:45,total:45,pr:'#30',time:'yesterday',minutes:1440}
];
