// W11 product UI. Every value on this page is read from the store through
// the same API the CLI uses. Two rules the code follows on purpose:
//
//  * a role is entered once and every call carries it — a 403 is reported
//    as a 403, not retried with a different token until something works;
//  * "filed" and "learned into" are rendered from two different fields,
//    because a correction that has been accepted is not yet learned.
'use strict';

const $ = (id) => document.getElementById(id);
const token = () => $('token').value.trim();

// One request helper: a failure is a real class plus detail, and the
// detail is what makes a refusal actionable instead of mysterious.
async function api(method, path, body) {
  const headers = {};
  if (token()) headers['Authorization'] = `Bearer ${token()}`;
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  const response = await fetch(path, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  let payload = null;
  try { payload = text ? JSON.parse(text) : null; } catch (_) { payload = text; }
  if (!response.ok) {
    const err = new Error((payload && payload.detail) || text || response.statusText);
    err.status = response.status;
    err.class = (payload && payload.class) || 'http-error';
    throw err;
  }
  return payload;
}

const opId = (prefix) => `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}`;

function say(outputId, message, isError) {
  const el = $(outputId);
  el.textContent = message;
  el.className = isError ? 'error' : 'ok';
}

function cell(row, text, isHeader) {
  const td = document.createElement(isHeader ? 'th' : 'td');
  if (isHeader) td.scope = 'row';
  td.textContent = text;
  row.appendChild(td);
  return td;
}

function renderTable(id, rows, columns, emptyText) {
  const body = document.querySelector(`#${id} tbody`);
  body.replaceChildren();
  if (!rows.length) {
    const tr = document.createElement('tr');
    const td = document.createElement('td');
    td.colSpan = columns.length;
    td.textContent = emptyText;
    tr.appendChild(td);
    body.appendChild(tr);
    return;
  }
  for (const row of rows) {
    const tr = document.createElement('tr');
    for (const [label, value] of columns(row)) {
      const td = cell(tr, value);
      td.dataset.label = label;
    }
    body.appendChild(tr);
  }
}

// ── predictions ──────────────────────────────────────────────────

async function loadPredictions() {
  try {
    const rows = await api('GET', '/api/predictions');
    renderTable('predictions', rows, (p) => [
      ['Prediction', p.id],
      ['Snapshot', p.snapshot.slice(0, 12)],
      ['Output', p.output],
      ['Abstained', String(p.abstained)],
      ['Correct this', ''],
    ], 'No predictions recorded yet.');
    // The last column is an action, so it is built after the text cells
    // to keep the table keyboard-navigable.
    document.querySelectorAll('#predictions tbody tr').forEach((tr) => {
      if (tr.cells.length < 5) return;
      const id = tr.cells[0].textContent;
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.textContent = 'correct';
      btn.addEventListener('click', () => {
        $('sig-pred').value = id;
        $('sig-content').focus();
      });
      tr.cells[4].replaceChildren(btn);
    });
  } catch (e) {
    renderTable('predictions', [], () => [], `unavailable: ${e.message}`);
  }
}

// ── signals ──────────────────────────────────────────────────────

async function loadSignals() {
  try {
    const ids = await lastSignalIds();
    const rows = await Promise.all(ids.map(async (id) => {
      const s = await api('GET', `/api/signals/${encodeURIComponent(id)}`);
      return { id, ...s };
    }));
    renderTable('signals', rows, (s) => [
      ['Signal', s.id],
      ['Status', s.status],
      // filed ≠ learned: the two columns are the point
      ['In force', s.in_force ? 'yes' : 'no'],
      ['Learned into', s.learned_into.length ? s.learned_into.join(', ') : 'not yet'],
    ], 'No signals yet.');
  } catch (e) {
    renderTable('signals', [], () => [], `unavailable: ${e.message}`);
  }
}

// The API has no "list all signals" endpoint by design (a signal's target
// may be a permission-sensitive record), so the page remembers the ids it
// filed itself and asks about those.
const filedSignals = JSON.parse(sessionStorage.getItem('grove.signals') || '[]');
const lastSignalIds = async () => filedSignals.slice();

// ── runs and lineage ─────────────────────────────────────────────

async function loadRuns() {
  try {
    const rows = await api('GET', '/api/runs');
    renderTable('runs', rows, (r) => [
      ['Run', r.id],
      ['State', r.state],
      ['Steps', `${r.steps_consumed}/${r.steps_budget}`],
      ['Resumed from', r.resumed_from || '—'],
    ], 'No runs.');
  } catch (e) {
    renderTable('runs', [], () => [], `unavailable: ${e.message}`);
  }
}

async function loadLineage() {
  const list = $('lineage');
  try {
    const data = await api('GET', '/api/lineage');
    list.replaceChildren();
    const checkpoints = data.nodes.filter((n) => n.kind === 'checkpoint');
    const snapshots = data.nodes.filter((n) => n.kind === 'snapshot');

    for (const node of snapshots) {
      const li = document.createElement('li');
      const head = document.createElement('strong');
      head.textContent = node.id;
      li.appendChild(head);
      li.appendChild(document.createTextNode(
        ` — ${node.snapshot.slice(0, 12)} ` +
        (node.deployable ? '(deployable)' : '(INVALIDATED — not deployable)'),
      ));
      if (node.revoked_signals.length) {
        const why = document.createElement('span');
        why.className = 'error';
        why.textContent = ` revoked signal(s): ${node.revoked_signals.join(', ')}`;
        li.appendChild(why);
      }
      if (node.parents.length) {
        const ul = document.createElement('ul');
        for (const edge of node.parents) {
          const sub = document.createElement('li');
          sub.textContent = `via ${edge.derivation} ← ${edge.parent.slice(0, 12)}`;
          ul.appendChild(sub);
        }
        li.appendChild(ul);
      }
      list.appendChild(li);
    }

    for (const cp of checkpoints) {
      const li = document.createElement('li');
      li.className = 'checkpoint';
      li.textContent =
        `checkpoint ${cp.id} — run ${cp.run}, ${cp.steps} steps, ` +
        `${cp.resume_level}, ` +
        (cp.recoverable ? 'recoverable' : 'NOT RECOVERABLE (state artifact gone)');
      list.appendChild(li);
    }

    if (!checkpoints.length && !snapshots.length) {
      const li = document.createElement('li');
      li.textContent = 'No lineage recorded yet.';
      list.appendChild(li);
    }

    const pub = data.publication
      ? `Active publication: v${data.publication.version} → ${data.publication.snapshot.slice(0, 12)}`
      : 'No active publication.';
    $('publication').textContent = pub;
  } catch (e) {
    list.replaceChildren();
    const li = document.createElement('li');
    li.textContent = `unavailable: ${e.message}`;
    list.appendChild(li);
  }
}

// ── modules ──────────────────────────────────────────────────────

async function loadModules() {
  const list = $('modules');
  try {
    const data = await api('GET', '/api/modules');
    list.replaceChildren();
    for (const snap of data.snapshots) {
      const li = document.createElement('li');
      const head = document.createElement('strong');
      head.textContent = snap.name;
      li.appendChild(head);
      li.appendChild(document.createTextNode(' — '));
      if (!snap.loadable) {
        const bad = document.createElement('span');
        bad.className = 'error';
        bad.textContent = ' — snapshot not loadable';
        li.appendChild(bad);
        list.appendChild(li);
        continue;
      }
      if (!snap.deployable) {
        const bad = document.createElement('span');
        bad.className = 'error';
        bad.textContent = ` — invalidated by ${snap.revoked_signals.join(', ')}`;
        li.appendChild(bad);
      }
      if ((snap.modules && snap.modules.length) || snap.ensemble) {
        const sub = document.createElement('ul');
        for (const m of snap.modules || []) {
          const item = document.createElement('li');
          item.textContent =
            `${m.name}: ${m.input_space} → ${m.output_space}` +
            (m.frozen ? ' (frozen)' : '') +
            (m.shared_group ? ` [shared: ${m.shared_group}]` : '') +
            ` · requires ${m.requires}` +
            (m.depends_on.length ? ` · depends on ${m.depends_on.join(', ')}` : '');
          sub.appendChild(item);
        }
        if (snap.ensemble) {
          const item = document.createElement('li');
          item.textContent =
            `ensemble (${snap.ensemble.rule}) in ${snap.ensemble.output_space}, ` +
            `budget ${snap.ensemble.budget_per_call} steps/call: ` +
            snap.ensemble.experts.map((e) => `${e.name}(${e.output_space})`).join(', ');
          sub.appendChild(item);
        }
        li.appendChild(sub);
      }
      list.appendChild(li);
    }
    if (!data.snapshots.length) {
      const li = document.createElement('li');
      li.textContent = 'No named snapshots yet.';
      list.appendChild(li);
    }
  } catch (e) {
    list.replaceChildren();
    const li = document.createElement('li');
    li.textContent = `unavailable: ${e.message}`;
    list.appendChild(li);
  }
}

// ── compare ──────────────────────────────────────────────────────

async function compare(event) {
  event.preventDefault();
  const snapshots = $('sel-snapshots').value.split(',').map((s) => s.trim()).filter(Boolean);
  try {
    const data = await api('POST', '/api/learning/select', {
      protocol: $('sel-protocol').value.trim(),
      snapshots,
    });
    renderTable('comparison', data.rows, (r) => [
      ['Snapshot', r.snapshot.slice(0, 12)],
      ['Repeats', String(r.repeats)],
      ['Mean', r.mean.map((m) => `${m.metric}=${m.value.toFixed(4)}`).join(' ')],
      ['Gates', r.meets_gates ? 'met' : `FAILED: ${r.gate_failures.join('; ')}`],
    ], 'Nothing compared yet.');
    say('select-out',
      `non-dominated: ${data.non_dominated.map((d) => d.slice(0, 12)).join(', ') || 'none'}`,
      false);
  } catch (e) {
    say('select-out', `${e.class}: ${e.message}`, true);
  }
}

// ── publish ──────────────────────────────────────────────────────

async function publish(event) {
  event.preventDefault();
  // A publication flips what every reader sees. It gets a confirmation
  // like any other irreversible action on a control surface.
  const snapshot = $('pub-snapshot').value.trim();
  if (!window.confirm(
    `Point the deployment at ${snapshot.slice(0, 12)}?\n\n` +
    'This replaces the active model for every reader.',
  )) return;
  const expected = $('pub-version').value.trim();
  try {
    const receipt = await api('POST', '/api/publish', {
      operation_id: opId('publish'),
      snapshot,
      protocol: $('pub-protocol').value.trim(),
      expected_version: expected === '' ? null : Number(expected),
    });
    say('publish-out', `published: v${receipt.version} is active`, false);
    await loadLineage();
  } catch (e) {
    say('publish-out', `${e.class}: ${e.message}`, true);
  }
}

async function predict(event) {
  event.preventDefault();
  const btn = document.querySelector('#predict-form button[type=submit]');
  btn.disabled = true;
  say('predict-out', 'running the model…', false);
  try {
    // no graph is sent: the snapshot carries its own, so the answer is
    // these weights under the structure they were trained with
    const receipt = await api('POST', '/api/predictions', {
      id: `p-${Date.now()}`,
      observation_id: $('pred-observation').value.trim(),
      snapshot: $('pred-snapshot').value.trim(),
    });
    const answer = receipt.abstained
      ? 'abstained'
      : `class ${receipt.output}`;
    say('predict-out',
      `${answer} (${receipt.id}, snapshot ${receipt.snapshot.slice(0, 12)})`, false);
    await loadPredictions();
  } catch (e) {
    say('predict-out', `${e.class}: ${e.message}`, true);
  } finally {
    btn.disabled = false;
  }
}

async function useActiveModel() {
  try {
    const data = await api('GET', '/api/lineage');
    if (data.publication) {
      $('pred-snapshot').value = data.publication.snapshot;
      say('predict-out', `active model v${data.publication.version} selected`, false);
    } else {
      say('predict-out', 'no active publication; paste a snapshot digest', true);
    }
  } catch (e) {
    say('predict-out', `${e.class}: ${e.message}`, true);
  }
}

// ── forms ────────────────────────────────────────────────────────

// The frozen task's image: a 16x16 grayscale field holding a horizontal
// or vertical bar. Synthesised here so the operator can see exactly what
// the model is about to look at — an uploaded blob would make the
// prediction unfalsifiable.
function synthPixels(orientation) {
  const SIDE = 16, BAR_LEN = 10, BAR_THICK = 3, BRIGHT = 220;
  const pixels = new Array(SIDE * SIDE).fill(12);
  for (let i = 0; i < SIDE; i++) {
    for (let j = 0; j < BAR_LEN; j++) {
      const row = orientation === 'horizontal' ? i : j;
      const col = orientation === 'horizontal' ? j : i;
      if (row >= 3 && row < 3 + BAR_THICK) {
        pixels[row * SIDE + col] = BRIGHT;
      }
    }
  }
  return pixels;
}

async function observe(event) {
  event.preventDefault();
  const mask = $('obs-mask').value.split(',').map((s) => s.trim() === 'true');
  const readings = $('obs-readings').value.split(',')
    .map((s) => Number(s.trim()))
    .filter((n) => Number.isFinite(n));
  try {
    const receipt = await api('POST', '/api/observations', {
      operation_id: opId('obs'),
      id: $('obs-id').value.trim(),
      task: $('obs-task').value.trim(),
      source: 'web-ui',
      modality_mask: mask,
      scene_id: 1,
      pixels: synthPixels($('obs-bar').value),
      readings,
    });
    say('observe-out', `observation ${receipt.id ?? ''} accepted`, false);
    // the predict control follows the observation just recorded: a form
    // that still points at the previous id would answer a question
    // nobody asked, and the answer would look equally authoritative
    $('pred-observation').value = receipt.id ?? $('obs-id').value.trim();
  } catch (e) {
    say('observe-out', `${e.class}: ${e.message}`, true);
  }
}

async function submitSignal(event) {
  event.preventDefault();
  const id = $('sig-id').value.trim();
  const prediction = $('sig-pred').value.trim();
  try {
    const receipt = await api('POST', '/api/signals', {
      operation_id: opId('sig'),
      id,
      kind: $('sig-kind').value,
      observation_id: null,
      prediction_id: prediction || null,
      target_field: $('sig-field').value.trim() || null,
      content: $('sig-content').value.trim(),
      usage_permitted: true,
    });
    filedSignals.push(id);
    sessionStorage.setItem('grove.signals', JSON.stringify(filedSignals));
    say('signal-out', `signal ${receipt.id ?? id} ${receipt.status}`, false);
    await loadSignals();
  } catch (e) {
    say('signal-out', `${e.class}: ${e.message}`, true);
  }
}

// ── wiring ───────────────────────────────────────────────────────

$('observe-form').addEventListener('submit', observe);
$('predict-form').addEventListener('submit', predict);
$('use-active').addEventListener('click', useActiveModel);
$('signal-form').addEventListener('submit', submitSignal);
$('select-form').addEventListener('submit', compare);
$('publish-form').addEventListener('submit', publish);
$('refresh').addEventListener('click', () => {
  loadPredictions(); loadSignals(); loadRuns(); loadLineage(); loadModules();
});
