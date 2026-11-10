import { parseOraclePlan, type OraclePlanRow } from './plan';

/** Parse the tabular output of DBMS_XPLAN.DISPLAY and DISPLAY_CURSOR. */
export function parseOracleText(text: string) {
  const lines = text.split(/\r?\n/);
  const header = lines.findIndex((line) => /^\s*\|\s*Id\s*\|\s*Operation\s*\|/i.test(line));
  if (header < 0) throw new Error('Oracle DBMS_XPLAN table header not found');
  const names = lines[header].split('|').slice(1, -1).map((name) => name.trim().toLowerCase());
  const rows: OraclePlanRow[] = [];
  const parents: Array<{ id: number; indent: number }> = [];
  const numeric = (value: string | undefined): number | null => {
    if (!value) return null;
    const match = value.trim().match(/^(\d+(?:\.\d+)?)([KMG])?/i);
    if (!match) return null;
    const unit = (match[2] ?? '').toUpperCase();
    return Number(match[1]) * (unit === 'K' ? 1000 : unit === 'M' ? 1e6 : unit === 'G' ? 1e9 : 1);
  };
  const elapsed = (value: string | undefined): number | null => {
    const match = value?.trim().match(/^(\d+):(\d+):(\d+(?:\.\d+)?)$/);
    return match ? (Number(match[1]) * 3600 + Number(match[2]) * 60 + Number(match[3])) * 1e6 : null;
  };
  let finished = false;
  for (const line of lines.slice(header + 1)) {
    if (!/^\s*\|\s*\*?\s*\d+\s*\|/.test(line)) {
      if (rows.length) { finished = /^\s*[-+]+\s*$/.test(line); break; }
      continue;
    }
    const cells = line.split('|').slice(1, -1);
    const get = (name: string) => cells[names.indexOf(name)];
    const id = Number((get('id') ?? '').replace('*', '').trim());
    const rawOperation = get('operation') ?? '';
    const indent = rawOperation.match(/^\s*/)?.[0].length ?? 0;
    while (parents.length && parents[parents.length - 1].indent >= indent) parents.pop();
    const row: OraclePlanRow = {
      id, parent_id: parents.at(-1)?.id ?? null, operation: rawOperation.trim(),
      object_name: get('name')?.trim() || null,
      cost: numeric(get('cost (%cpu)')), cardinality: numeric(get('e-rows') ?? get('rows')),
      bytes: numeric(get('e-bytes') ?? get('bytes')), starts: numeric(get('starts')),
      actual_rows: numeric(get('a-rows')), elapsed_us: elapsed(get('a-time')),
      cr_buffer_gets: numeric(get('buffers')), disk_reads: numeric(get('reads')),
    };
    rows.push(row);
    parents.push({ id, indent });
  }
  if (!rows.length) throw new Error('Oracle DBMS_XPLAN table has no operations');
  if (!finished || new Set(rows.map(row => row.id)).size !== rows.length) throw new Error('Oracle DBMS_XPLAN table is incomplete or has duplicate IDs');
  const predicateSection = text.split(/Predicate Information \(identified by operation id\):/i)[1]?.split(/\n\s*(?:Note|Column Projection Information|Query Block Name|Outline Data)\s*\n/i)[0] ?? '';
  const predicates = [...predicateSection.matchAll(/^\s*(\d+)\s*-\s*(access|filter)\(([^\n]*)/gmi)];
  for (const predicate of predicates) {
    const row = rows.find((candidate) => candidate.id === Number(predicate[1]));
    if (!row) continue;
    const value = predicate[3].trim().replace(/\)$/, '');
    if (predicate[2].toLowerCase() === 'access') row.access_predicates = value;
    else row.filter_predicates = value;
  }
  const statistics = names.includes('a-rows');
  return parseOraclePlan(JSON.stringify({ version: 1, statistics, plan: rows, text }));
}
