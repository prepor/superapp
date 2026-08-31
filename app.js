// superapp — ui prototype, iteration 2
// panels on a horizontally scrolling 12×6 workspace (niri-style columns)
// alt = workspace modifier (panels); plain keys belong to the focused panel

'use strict';

const GRID_W = 12;
const GRID_H = 6;
const GAP = 8;

/* ================= fake data ================= */

const ME = 'me@prepor.dev';

const EMAILS = [
  {
    id: 'm1', from: { name: 'Vera Kovac', email: 'vera@kovac.io' },
    subject: 'Q3 infra budget draft', date: 'aug 31 09:14', unread: true,
    body: [
      'Draft for Q3 infra spend is ready. Main deltas: the old staging cluster goes away and CI runners move to the new box.',
      'Can you sanity-check the numbers before Thursday? Especially egress — I suspect the CDN line is stale.',
    ],
  },
  {
    id: 'm2', from: { name: 'GitHub', email: 'notifications@github.com' },
    subject: '[stelaxis] CI failed on main', date: 'aug 31 08:02', unread: true,
    statusLine: { text: 'ci: FAILED — build (2m 14s), tests (41s)', error: true },
    body: [
      'Workflow main #4128 failed on push 9f3c2a1.',
      'Failed steps: mix test (2 failures), credo --strict (1 warning). Full logs are attached to the run.',
    ],
  },
  {
    id: 'm3', from: { name: 'Max Ivanov', email: 'max@ivanov.dev' },
    subject: 'Re: superapp panel model', date: 'aug 30 22:47', unread: false,
    body: [
      'Read your note on panels. The joined/replace rule feels like the right default — it is the preview-pane pattern, but generalized to everything.',
      'One question though: what happens to a half-written draft if a joined compose panel gets replaced by the next link? Feels like some panels need a way to resist replacement.',
    ],
  },
  {
    id: 'm4', from: { name: 'Elena Petrova', email: 'elena.p@gmail.com' },
    subject: 'Sat hike — early start?', date: 'aug 30 18:20', unread: false,
    body: [
      'Weather looks fine for Saturday. Early start (7:30) or lazy start (10:00)?',
      'There is a new trail variant, ~14 km, one café stop. Bring the good thermos.',
    ],
  },
  {
    id: 'm5', from: { name: 'RSS Digest', email: 'digest@rss.local' },
    subject: 'weekly: 14 unread items in 3 feeds', date: 'aug 30 07:00', unread: false,
    body: [
      'Unread this week: niri release notes (2), simonwillison.net (9), lobste.rs top (3).',
      'This digest is itself a candidate for an rss/feed panel, by the way.',
    ],
  },
  {
    id: 'm6', from: { name: 'Calendar', email: 'calendar@local' },
    subject: 'invite: dentist — tue 10:00', date: 'aug 29 16:41', unread: false,
    body: [
      'Dentist, Tuesday 10:00–10:45. Reminder set for 30 minutes before.',
      'Reply yes to confirm, or propose a new time.',
    ],
  },
  {
    id: 'm7', from: { name: 'Hetzner', email: 'billing@hetzner.com' },
    subject: 'invoice 2026-08 — €46.20', date: 'aug 29 11:05', unread: false,
    body: [
      'Invoice 2026-08 for €46.20 is available. Auto-charge on Sep 3.',
      'Usage: 2× CX32, 1× volume 100 GB, egress 214 GB.',
    ],
  },
  {
    id: 'm8', from: { name: 'Dmitry Orlov', email: 'dorlov@fastmail.com' },
    subject: 'that airport book', date: 'aug 28 20:33', unread: false,
    body: [
      'Found it — the airport design book you mentioned at dinner. Ordering a copy tomorrow.',
      'Borrowing rights claimed for after you finish, obviously.',
    ],
  },
];

const email = (id) => EMAILS.find((m) => m.id === id);
const inboxList = () => EMAILS.filter((m) => !m.archived);

/* ================= tiny dom helper ================= */

function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v == null) continue;
    if (k === 'class') el.className = v;
    else if (k === 'style') el.style.cssText = v;
    else if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else el.setAttribute(k, v);
  }
  for (const c of children.flat()) {
    if (c == null || c === false) continue;
    el.append(c.nodeType ? c : document.createTextNode(c));
  }
  return el;
}

/* ================= state ================= */

let nextId = 1;

const state = {
  columns: [],          // [{id, panels: [panelId, ...]}] left → right
  panels: new Map(),    // panelId -> {id, kind, params, w, h, ui}
  joins: new Map(),     // parent panelId -> joined child panelId
  focus: null,
};

function locate(pid) {
  for (let c = 0; c < state.columns.length; c++) {
    const r = state.columns[c].panels.indexOf(pid);
    if (r !== -1) return { colIdx: c, rowIdx: r };
  }
  return null;
}

const colIdxOf = (pid) => locate(pid)?.colIdx ?? -1;
const colUsedH = (col) => col.panels.reduce((s, pid) => s + state.panels.get(pid).h, 0);
const colW = (col) => Math.max(...col.panels.map((pid) => state.panels.get(pid).w));

// a join is alive only while the child sits in the column immediately right
// of its parent — any move or insert that breaks adjacency breaks the join
function validateJoins() {
  for (const [a, b] of [...state.joins]) {
    if (!state.panels.has(a) || !state.panels.has(b) || colIdxOf(b) !== colIdxOf(a) + 1) {
      state.joins.delete(a);
    }
  }
}

/* ================= actions ================= */

function mkPanel(kind, params) {
  const def = kinds[kind];
  const p = { id: 'p' + nextId++, kind, params, w: def.w, h: def.h, ui: {} };
  state.panels.set(p.id, p);
  return p;
}

function removePanelFromLayout(pid) {
  const loc = locate(pid);
  if (!loc) return;
  const col = state.columns[loc.colIdx];
  col.panels.splice(loc.rowIdx, 1);
  if (!col.panels.length) state.columns.splice(loc.colIdx, 1);
}

// replacing a panel closes everything joined to it, transitively — the
// chain to its right is context derived from content that just changed
function closeJoinedChain(pid) {
  const child = state.joins.get(pid);
  if (!child) return;
  state.joins.delete(pid);
  closeJoinedChain(child);
  removePanelFromLayout(child);
  state.panels.delete(child);
}

function openPanel(kind, params, opts = {}) {
  const p = mkPanel(kind, params);
  const fromLoc = opts.from ? locate(opts.from.id) : null;
  if (fromLoc) {
    placeNear(p, fromLoc.colIdx, !!opts.join);
    if (opts.join) state.joins.set(opts.from.id, p.id);
  } else {
    state.columns.push({ id: 'c' + nextId++, panels: [p.id] });
  }
  kinds[kind].onShow?.(params);
  validateJoins();
  state.focus = p.id;
  render();
}

// "open to the right": reuse the right-hand column if the new panel's rows
// fit, otherwise insert a fresh column. A joined child must land in the
// column immediately right of its parent (a join only lives there); an
// un-joined open respects an existing pair and goes after it instead.
function placeNear(p, fromIdx, joined) {
  const right = state.columns[fromIdx + 1];
  if (right && colUsedH(right) + p.h <= GRID_H) {
    right.panels.push(p.id);
    return;
  }
  let at = fromIdx + 1;
  if (!joined) {
    for (const pid of state.columns[fromIdx].panels) {
      const child = state.joins.get(pid);
      if (child && colIdxOf(child) === fromIdx + 1) at = fromIdx + 2;
    }
  }
  state.columns.splice(at, 0, { id: 'c' + nextId++, panels: [p.id] });
}

function replacePanel(pid, kind, params) {
  const p = state.panels.get(pid);
  if (!p) return;
  closeJoinedChain(pid);
  const def = kinds[kind];
  p.kind = kind;
  p.params = params;
  p.w = def.w;
  p.h = def.h;
  p.ui = {};
  def.onShow?.(params);
  validateJoins();
  state.focus = pid;
  render();
}

function closePanel(pid) {
  const loc = locate(pid);
  if (!loc) return;
  removePanelFromLayout(pid);
  state.panels.delete(pid);
  for (const [a, b] of [...state.joins]) if (a === pid || b === pid) state.joins.delete(a);
  validateJoins();
  if (state.focus === pid) {
    state.focus = null;
    const ci = Math.min(loc.colIdx, state.columns.length - 1);
    if (ci >= 0) {
      const c = state.columns[ci];
      state.focus = c.panels[Math.min(loc.rowIdx, c.panels.length - 1)];
    }
  }
  render();
}

function movePanel(pid, dir) {
  const loc = locate(pid);
  if (!loc) return;
  const { colIdx, rowIdx } = loc;
  const col = state.columns[colIdx];
  if (dir === 'up' || dir === 'down') {
    const t = rowIdx + (dir === 'up' ? -1 : 1);
    if (t < 0 || t >= col.panels.length) return;
    [col.panels[rowIdx], col.panels[t]] = [col.panels[t], col.panels[rowIdx]];
  } else {
    const d = dir === 'left' ? -1 : 1;
    const t = colIdx + d;
    if (col.panels.length === 1) {
      if (t < 0 || t >= state.columns.length) return; // lone column at the edge
      [state.columns[colIdx], state.columns[t]] = [state.columns[t], state.columns[colIdx]];
    } else {
      col.panels.splice(rowIdx, 1);
      if (t < 0 || t >= state.columns.length) {
        state.columns.splice(d === -1 ? colIdx : colIdx + 1, 0, { id: 'c' + nextId++, panels: [pid] });
      } else {
        const tc = state.columns[t];
        tc.panels.splice(Math.min(rowIdx, tc.panels.length), 0, pid);
      }
    }
  }
  validateJoins();
  render();
}

/* ================= links & buttons =================
   solid underline  opens a joined panel — or re-targets the existing joined one
   dotted underline replaces the panel it lives in
   alt+click        always a fresh, un-joined panel
   button           side effect only, never navigation                       */

function link(panel, label, target, mode = 'open') {
  return h('a', {
    class: mode === 'open' ? 'lnk-open' : 'lnk-replace',
    href: '#',
    title: mode === 'open'
      ? 'opens a joined panel to the right · alt+click: separate panel'
      : 'replaces this panel · alt+click: separate panel',
    onclick: (e) => {
      e.preventDefault();
      followLink(panel, target, mode, e.altKey);
    },
  }, label);
}

function followLink(panel, target, mode, alt) {
  if (alt) return openPanel(target.kind, target.params, { from: panel, join: false });
  if (mode === 'replace') return replacePanel(panel.id, target.kind, target.params);
  const joined = state.joins.get(panel.id);
  if (joined && state.panels.has(joined)) return replacePanel(joined, target.kind, target.params);
  openPanel(target.kind, target.params, { from: panel, join: true });
}

function btn(label, onclick, title) {
  return h('button', {
    title: title ?? null,
    onclick: (e) => { e.stopPropagation(); onclick(e); },
  }, label);
}

/* ================= panel kinds ================= */

function inboxVisible(p) {
  const q = (p.ui.filter ?? '').trim().toLowerCase();
  return inboxList().filter((m) =>
    !q || `${m.from.name} ${m.from.email} ${m.subject}`.toLowerCase().includes(q));
}

const kinds = {

  'help': {
    w: 4, h: 6,
    title: () => 'help',
    render(body, p) {
      const about = { kind: 'about', params: {} };
      body.append(h('div', { class: 'help' },
        h('h3', {}, 'legend'),
        h('ul', {},
          h('li', {}, link(p, 'solid underline', about, 'open'), ' — opens a new panel to the right, joined to this one'),
          h('li', {}, link(p, 'dotted underline', about, 'replace'), ' — replaces this panel in place'),
          h('li', {}, btn('button', () => status('side effect: nothing was opened or replaced')), ' — side effect only, never navigation'),
          h('li', {}, h('kbd', {}, 'alt'), '+click or ', h('kbd', {}, 'alt'), '+enter — always a fresh, un-joined panel'),
          h('li', {}, 'a ═ bridge marks a joined pair: the next solid link in the parent replaces the joined panel, and replacing a panel closes its joined chain. joins live only between adjacent columns — move a panel away and it un-joins'),
          h('li', {}, 'color is reserved for errors: ', h('span', { class: 'error' }, 'like this')),
        ),
        h('h3', {}, 'keys'),
        h('ul', {},
          h('li', {}, h('kbd', {}, 'alt'), '+', h('kbd', {}, '←'), h('kbd', {}, '↓'), h('kbd', {}, '↑'), h('kbd', {}, '→'), ' or ', h('kbd', {}, 'alt'), '+', h('kbd', {}, 'h'), h('kbd', {}, 'j'), h('kbd', {}, 'k'), h('kbd', {}, 'l'), ' — focus panels'),
          h('li', {}, h('kbd', {}, 'alt'), '+', h('kbd', {}, 'shift'), '+ the same — move the focused panel'),
          h('li', {}, h('kbd', {}, 'alt'), '+', h('kbd', {}, 'x'), ' — close the focused panel'),
          h('li', {}, 'plain keys belong to the focused panel — inbox: ', h('kbd', {}, 'j'), h('kbd', {}, 'k'), h('kbd', {}, 'enter'), h('kbd', {}, '/'), ' · message: ', h('kbd', {}, 'j'), h('kbd', {}, 'k'), h('kbd', {}, 'r')),
          h('li', {}, h('kbd', {}, 'esc'), ' — leave a text field · mouse works for everything'),
        ),
        h('h3', {}, 'try'),
        h('ol', {},
          h('li', {}, 'click a subject in the inbox — a message opens, joined (bridge)'),
          h('li', {}, 'j / k and enter in the inbox — the same, by keyboard'),
          h('li', {}, 'click another subject — it replaces the joined message'),
          h('li', {}, 'in a message: from → contact joins the chain; the next subject click closes the whole chain'),
          h('li', {}, 'alt+shift+← the message — moved away from its parent, it un-joins'),
        ),
      ));
    },
  },

  'about': {
    w: 3, h: 2,
    title: () => 'about',
    render(body, p) {
      body.append(
        h('p', { style: 'margin:0 0 8px' },
          'superapp — ui prototype, iteration 2. no apps, no windows: specialized panels on one scrolling 12×6 workspace.'),
        h('p', { style: 'margin:0' }, link(p, 'back to help', { kind: 'help', params: {} }, 'replace')),
      );
    },
  },

  'email/inbox': {
    w: 4, h: 6,
    title: (params) => params.filter ? `inbox · ${params.filter}` : 'inbox',
    headerActions: (p) => [btn('refresh', () => status('inbox refreshed (fake)'))],
    render(body, p) {
      p.ui.filter ??= p.params.filter ?? '';
      const tbody = h('tbody');
      const renderRows = () => {
        tbody.textContent = '';
        const rows = inboxVisible(p);
        for (const m of rows) {
          const tr = h('tr', {
            class: (m.unread ? 'unread' : '') + (p.ui.sel === m.id ? ' sel' : ''),
            onclick: () => {
              p.ui.sel = m.id;
              for (const r of tbody.children) r.classList.remove('sel');
              tr.classList.add('sel');
            },
          },
            h('td', { class: 'rt-trunc', title: m.from.name }, m.from.name),
            h('td', { class: 'rt-trunc' }, link(p, m.subject, { kind: 'email/message', params: { id: m.id } }, 'open')),
            h('td', { class: 'rt-right rt-date' }, m.date));
          tbody.append(tr);
        }
        if (!rows.length) tbody.append(h('tr', {}, h('td', { class: 'rt-empty', colspan: 3 }, 'no messages')));
        tbody.querySelector('tr.sel')?.scrollIntoView({ block: 'nearest' });
      };
      p.ui.renderRows = renderRows;
      const filter = h('input', {
        class: 'rt-filter', placeholder: 'filter…  ( / )', value: p.ui.filter, spellcheck: 'false',
        oninput: (e) => { p.ui.filter = e.target.value; renderRows(); },
        onkeydown: (e) => {
          if (e.key === 'Enter') {
            e.preventDefault();
            e.target.blur();
            const rows = inboxVisible(p);
            if (rows.length) { p.ui.sel = rows[0].id; renderRows(); }
          }
        },
      });
      p.ui.filterEl = filter;
      renderRows();
      body.append(h('div', { class: 'rt' },
        filter,
        h('div', { class: 'rt-scroll' },
          h('table', { class: 'rt-table' },
            h('colgroup', {}, h('col', { style: 'width:96px' }), h('col'), h('col', { style: 'width:86px' })),
            h('thead', {}, h('tr', {}, h('th', {}, 'from'), h('th', {}, 'subject'), h('th', { class: 'rt-right' }, 'date'))),
            tbody))));
    },
    onKey(p, e) {
      if (e.key === '/') { p.ui.filterEl?.focus(); return true; }
      const rows = inboxVisible(p);
      if (!rows.length) return false;
      const down = e.key === 'j' || e.key === 'ArrowDown';
      const up = e.key === 'k' || e.key === 'ArrowUp';
      if (down || up) {
        const idx = rows.findIndex((m) => m.id === p.ui.sel);
        const next = idx === -1
          ? (down ? 0 : rows.length - 1)
          : Math.max(0, Math.min(rows.length - 1, idx + (down ? 1 : -1)));
        p.ui.sel = rows[next].id;
        p.ui.renderRows?.();
        return true;
      }
      if (e.key === 'Enter') {
        const m = rows.find((r) => r.id === p.ui.sel) ?? rows[0];
        followLink(p, { kind: 'email/message', params: { id: m.id } }, 'open', e.altKey);
        return true;
      }
      return false;
    },
  },

  'email/message': {
    w: 4, h: 3,
    title: (params) => email(params.id)?.subject ?? 'message',
    onShow: (params) => { const m = email(params.id); if (m) m.unread = false; },
    headerActions: (p) => [btn('archive', () => {
      const m = email(p.params.id);
      if (m) { m.archived = true; status(`archived “${m.subject}” (fake)`); }
      closePanel(p.id);
    })],
    render(body, p) {
      const m = email(p.params.id);
      if (!m) { body.append(h('p', { class: 'muted', style: 'margin:0' }, 'message not found')); return; }
      const list = inboxList();
      const idx = list.indexOf(m);
      const newer = list[idx - 1];
      const older = list[idx + 1];
      body.append(
        h('dl', { class: 'meta' },
          h('dt', {}, 'from'),
          h('dd', {}, link(p, `${m.from.name} <${m.from.email}>`, { kind: 'contact', params: { email: m.from.email } }, 'open')),
          h('dt', {}, 'to'), h('dd', { class: 'muted' }, ME),
          h('dt', {}, 'date'), h('dd', {}, m.date)),
        m.statusLine
          ? h('p', { class: m.statusLine.error ? 'error' : '', style: 'margin:0 0 8px' }, m.statusLine.text)
          : null,
        h('div', { class: 'msg-body' }, m.body.map((t) => h('p', {}, t))),
        h('div', { class: 'msg-nav' },
          newer ? link(p, '← newer', { kind: 'email/message', params: { id: newer.id } }, 'replace')
                : h('span', { class: 'muted' }, '← newer'),
          older ? link(p, 'older →', { kind: 'email/message', params: { id: older.id } }, 'replace')
                : h('span', { class: 'muted' }, 'older →'),
          h('span', { class: 'spacer' }),
          link(p, 'reply', { kind: 'email/compose', params: { re: m.id } }, 'open')));
    },
    onKey(p, e) {
      const m = email(p.params.id);
      if (!m) return false;
      const list = inboxList();
      const idx = list.indexOf(m);
      if (e.key === 'j' || e.key === 'k') {
        const t = list[idx + (e.key === 'j' ? 1 : -1)];
        if (t) followLink(p, { kind: 'email/message', params: { id: t.id } }, 'replace', e.altKey);
        return true;
      }
      if (e.key === 'r') {
        followLink(p, { kind: 'email/compose', params: { re: m.id } }, 'open', e.altKey);
        return true;
      }
      return false;
    },
  },

  'contact': {
    w: 3, h: 2,
    title: (params) => EMAILS.find((m) => m.from.email === params.email)?.from.name ?? params.email,
    render(body, p) {
      const all = EMAILS.filter((m) => m.from.email === p.params.email);
      const name = all[0]?.from.name ?? p.params.email;
      body.append(
        h('div', { class: 'big' }, name),
        h('div', { class: 'muted', style: 'margin-bottom:8px' }, p.params.email),
        h('div', {}, `${all.length} message(s) in mail`),
        h('div', { style: 'margin-top:8px' },
          link(p, `messages from ${name.split(' ')[0].toLowerCase()}`,
            { kind: 'email/inbox', params: { filter: p.params.email } }, 'open')));
    },
  },

  'email/compose': {
    w: 4, h: 4,
    title: (params) => params.re ? `re: ${email(params.re)?.subject ?? ''}` : 'new mail',
    render(body, p) {
      const m = p.params.re ? email(p.params.re) : null;
      p.ui.to ??= m ? m.from.email : '';
      p.ui.subject ??= m ? `Re: ${m.subject}` : '';
      p.ui.text ??= '';
      const ta = h('textarea', { placeholder: 'write…', oninput: (e) => p.ui.text = e.target.value }, p.ui.text);
      body.append(h('div', { class: 'form' },
        h('div', { class: 'frow' }, h('label', {}, 'to'),
          h('input', { value: p.ui.to, spellcheck: 'false', oninput: (e) => p.ui.to = e.target.value })),
        h('div', { class: 'frow' }, h('label', {}, 'subject'),
          h('input', { value: p.ui.subject, spellcheck: 'false', oninput: (e) => p.ui.subject = e.target.value })),
        h('div', { class: 'f-grow' }, ta),
        h('div', { class: 'f-btns' },
          btn('discard', () => closePanel(p.id)),
          btn('send', () => { status(`sent to ${p.ui.to} (fake)`); closePanel(p.id); }))));
      if (!p.ui.focusedOnce) {
        p.ui.focusedOnce = true;
        requestAnimationFrame(() => ta.focus());
      }
    },
  },
};

/* ================= rendering ================= */

const wsEl = () => document.getElementById('workspace');
const stripEl = () => document.getElementById('strip');

function applyFocus() {
  for (const el of document.querySelectorAll('.panel')) {
    el.classList.toggle('focused', el.dataset.pid === state.focus);
  }
}

function renderPanel(p, rowU) {
  const def = kinds[p.kind];
  const el = h('div', {
    class: 'panel', 'data-pid': p.id,
    style: `height:${Math.round(p.h * rowU + (p.h - 1) * GAP)}px`,
    onmousedown: () => { if (state.focus !== p.id) { state.focus = p.id; applyFocus(); } },
  });
  const title = def.title(p.params);
  el.append(h('div', { class: 'p-head' },
    h('span', { class: 'p-title', title }, title),
    h('span', { class: 'p-acts' },
      def.headerActions ? def.headerActions(p) : null,
      h('button', {
        class: 'p-close', title: 'close panel',
        onclick: (e) => { e.stopPropagation(); closePanel(p.id); },
      }, '×'))));
  const bodyEl = h('div', { class: 'p-body', 'data-pid': p.id });
  def.render(bodyEl, p);
  el.append(bodyEl);
  return el;
}

function render() {
  const ws = wsEl(), strip = stripEl();
  const scrollLeft = ws.scrollLeft;
  const bodyScrolls = {};
  for (const b of strip.querySelectorAll('.p-body')) bodyScrolls[b.dataset.pid] = b.scrollTop;

  // keep dom focus (and caret) in text fields across rebuilds
  const ae = document.activeElement;
  let refocus = null;
  if (ae && ae.matches?.('input, textarea')) {
    const bodyEl = ae.closest('.p-body');
    if (bodyEl) {
      refocus = {
        pid: bodyEl.dataset.pid,
        idx: [...bodyEl.querySelectorAll('input, textarea')].indexOf(ae),
        start: ae.selectionStart, end: ae.selectionEnd,
      };
    }
  }

  strip.textContent = '';
  const unit = (ws.clientWidth - GAP) / GRID_W;                    // grid col width, gap-adjusted
  const rowU = (ws.clientHeight - 2 * GAP - (GRID_H - 1) * GAP) / GRID_H; // grid row height
  for (const col of state.columns) {
    const colEl = h('div', { class: 'col', style: `width:${Math.max(40, Math.round(unit * colW(col) - GAP))}px` });
    for (const pid of col.panels) colEl.append(renderPanel(state.panels.get(pid), rowU));
    strip.append(colEl);
  }
  strip.append(h('div', { id: 'overlay' }));

  for (const b of strip.querySelectorAll('.p-body')) {
    if (bodyScrolls[b.dataset.pid] != null) b.scrollTop = bodyScrolls[b.dataset.pid];
  }
  ws.scrollLeft = scrollLeft;
  if (refocus) {
    const nb = strip.querySelector(`.p-body[data-pid="${refocus.pid}"]`);
    const f = nb && [...nb.querySelectorAll('input, textarea')][refocus.idx];
    if (f) {
      f.focus();
      try { f.setSelectionRange(refocus.start, refocus.end); } catch { /* number inputs etc. */ }
    }
  }
  applyFocus();
  requestAnimationFrame(() => { drawBridges(); scrollFocusIntoView(); });
}

// the bridge is the join indicator: drawn for every live join (validateJoins
// guarantees the pair is column-adjacent), at the child's header when
// possible, falling back so a join is never invisible
function drawBridges() {
  const ov = document.getElementById('overlay');
  if (!ov) return;
  ov.textContent = '';
  const sr = stripEl().getBoundingClientRect();
  for (const [a, b] of state.joins) {
    const ea = document.querySelector(`.panel[data-pid="${a}"]`);
    const eb = document.querySelector(`.panel[data-pid="${b}"]`);
    if (!ea || !eb) continue;
    const ra = ea.getBoundingClientRect(), rb = eb.getBoundingClientRect();
    let y = rb.top + 13;                                  // child header level
    if (y < ra.top || y > ra.bottom) {
      y = ra.top + 13;                                    // parent header level
      if (y < rb.top || y > rb.bottom) {
        const top = Math.max(ra.top, rb.top), bot = Math.min(ra.bottom, rb.bottom);
        y = top < bot ? (top + bot) / 2 : rb.top + 13;    // overlap midpoint, else child header
      }
    }
    const w = rb.left - ra.right;
    if (w <= 0 || w > 60) continue;
    ov.append(h('div', {
      class: 'bridge',
      style: `left:${ra.right - sr.left}px;top:${y - sr.top - 2}px;width:${w}px`,
    }));
  }
}

function scrollFocusIntoView() {
  if (!state.focus) return;
  document.querySelector(`.panel[data-pid="${state.focus}"]`)
    ?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
}

/* ================= toast (side-effect feedback) ================= */

let toastTimer;
function status(msg, isError) {
  let t = document.getElementById('toast');
  if (!t) { t = h('div', { id: 'toast' }); document.body.append(t); }
  t.textContent = msg;
  t.classList.toggle('error', !!isError);
  t.style.display = 'block';
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { t.style.display = 'none'; }, 3000);
}

window.addEventListener('error', (e) => status(`error: ${e.message}`, true));

/* ================= keyboard ================= */

function focusDir(dir) {
  if (!state.focus) {
    if (state.columns[0]) { state.focus = state.columns[0].panels[0]; applyFocus(); scrollFocusIntoView(); }
    return;
  }
  const { colIdx, rowIdx } = locate(state.focus);
  const col = state.columns[colIdx];
  let target = null;
  if (dir === 'up' || dir === 'down') {
    target = col.panels[rowIdx + (dir === 'up' ? -1 : 1)] ?? null;
  } else {
    const tc = state.columns[colIdx + (dir === 'left' ? -1 : 1)];
    if (tc) {
      // nearest panel by vertical center
      const cur = document.querySelector(`.panel[data-pid="${state.focus}"]`)?.getBoundingClientRect();
      let best = Infinity;
      for (const pid of tc.panels) {
        const r = document.querySelector(`.panel[data-pid="${pid}"]`)?.getBoundingClientRect();
        const d = cur && r ? Math.abs((r.top + r.bottom) / 2 - (cur.top + cur.bottom) / 2) : 0;
        if (d < best) { best = d; target = pid; }
      }
    }
  }
  if (target) { state.focus = target; applyFocus(); scrollFocusIntoView(); }
}

// alt+key uses e.code: on macOS alt+letter mutates e.key (alt+h = '˙')
const WM_DIRS = {
  ArrowLeft: 'left', ArrowRight: 'right', ArrowUp: 'up', ArrowDown: 'down',
  KeyH: 'left', KeyJ: 'down', KeyK: 'up', KeyL: 'right',
};

window.addEventListener('keydown', (e) => {
  if (e.metaKey || e.ctrlKey) return;
  const inInput = e.target instanceof Element && e.target.matches('input, textarea');

  if (e.altKey) {
    const dir = WM_DIRS[e.code];
    if (dir) {
      e.preventDefault();
      if (inInput) e.target.blur();
      if (e.shiftKey) { if (state.focus) movePanel(state.focus, dir); }
      else focusDir(dir);
      return;
    }
    if (e.code === 'KeyX') {
      e.preventDefault();
      if (inInput) e.target.blur();
      if (state.focus) closePanel(state.focus);
      return;
    }
    // other alt combos fall through to the panel (e.g. alt+enter)
  }

  if (inInput) {
    if (e.key === 'Escape') e.target.blur();
    return;
  }

  const p = state.panels.get(state.focus);
  if (!p) return;
  if (kinds[p.kind].onKey?.(p, e)) { e.preventDefault(); return; }
  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    const b = document.querySelector(`.p-body[data-pid="${p.id}"]`);
    if (b) { b.scrollBy(0, e.key === 'ArrowDown' ? 60 : -60); e.preventDefault(); }
  }
});

window.addEventListener('resize', () => render());

/* ================= boot ================= */

function boot() {
  const help = mkPanel('help', {});
  const inbox = mkPanel('email/inbox', {});
  state.columns.push({ id: 'c' + nextId++, panels: [help.id] });
  state.columns.push({ id: 'c' + nextId++, panels: [inbox.id] });
  state.focus = inbox.id;
  render();
}

boot();
