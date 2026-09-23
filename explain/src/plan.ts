import type { ExplainNode, ExplainPlan } from "@tabularis/explain";

/**
 * One plan operation as captured by the Rust plugin, keyed by the lowercase
 * `PLAN_TABLE` / `V$SQL_PLAN_STATISTICS_ALL` column name.
 */
export interface OraclePlanRow {
  id: number;
  parent_id: number | null;
  depth?: number | null;
  position?: number | null;
  operation: string;
  options?: string | null;
  object_owner?: string | null;
  object_name?: string | null;
  object_alias?: string | null;
  object_type?: string | null;
  optimizer?: string | null;
  cost?: number | null;
  cardinality?: number | null;
  bytes?: number | null;
  cpu_cost?: number | null;
  io_cost?: number | null;
  time?: number | null;
  partition_start?: string | null;
  partition_stop?: string | null;
  access_predicates?: string | null;
  filter_predicates?: string | null;
  projection?: string | null;
  qblock_name?: string | null;
  /** Runtime statistics, present only in analyzed payloads (last execution). */
  starts?: number | null;
  actual_rows?: number | null;
  elapsed_us?: number | null;
  cr_buffer_gets?: number | null;
  cu_buffer_gets?: number | null;
  disk_reads?: number | null;
}

/** The `oracle-plan-json` wire payload. */
export interface OraclePlanPayload {
  version: number;
  statistics: boolean;
  plan: OraclePlanRow[];
  text?: string | null;
}

const SUPPORTED_VERSION = 1;

function asNumber(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  // Values beyond i64 arrive as strings to keep them exact.
  if (typeof value === "string" && value.trim() !== "") {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : null;
  }
  return null;
}

function asText(value: unknown): string | null {
  return typeof value === "string" && value.trim() !== "" ? value : null;
}

function isJoin(operation: string): boolean {
  return /JOIN|NESTED LOOPS/.test(operation);
}

/** Oracle reports A-Rows and A-Time as totals over all Starts; the shared model is per loop. */
function perLoop(total: number | null, loops: number | null): number | null {
  if (total == null) return null;
  return loops != null && loops > 0 ? total / loops : total;
}

function toNode(row: OraclePlanRow, statistics: boolean): ExplainNode {
  const operation = row.operation.trim();
  const options = asText(row.options);
  const access = asText(row.access_predicates);
  const starts = statistics ? asNumber(row.starts) : null;
  const totalRows = statistics ? asNumber(row.actual_rows) : null;
  const elapsedUs = statistics ? asNumber(row.elapsed_us) : null;
  const totalMs = elapsedUs == null ? null : elapsedUs / 1000;
  const crGets = statistics ? asNumber(row.cr_buffer_gets) : null;
  const cuGets = statistics ? asNumber(row.cu_buffer_gets) : null;

  const isIndexAccess = operation.startsWith("INDEX");
  const isHashLike = operation.startsWith("HASH JOIN") || operation.startsWith("MERGE JOIN");

  const extra: Record<string, unknown> = {};
  const addExtra = (key: string, value: unknown) => {
    if (value !== null && value !== undefined && value !== "") extra[key] = value;
  };
  addExtra("object_owner", asText(row.object_owner));
  addExtra("object_type", asText(row.object_type));
  addExtra("object_alias", asText(row.object_alias));
  addExtra("optimizer", asText(row.optimizer));
  addExtra("bytes", asNumber(row.bytes));
  addExtra("cpu_cost", asNumber(row.cpu_cost));
  addExtra("io_cost", asNumber(row.io_cost));
  addExtra("time_s", asNumber(row.time));
  addExtra("partition_start", asText(row.partition_start));
  addExtra("partition_stop", asText(row.partition_stop));
  addExtra("query_block", asText(row.qblock_name));
  addExtra("projection", asText(row.projection));
  if (access && !isIndexAccess && !isHashLike) addExtra("access_predicates", access);
  addExtra("actual_rows_total", totalRows);
  addExtra("actual_time_ms_total", totalMs);

  return {
    id: `oracle-${row.id}`,
    node_type: options ? `${operation} ${options}` : operation,
    relation: asText(row.object_name),
    startup_cost: null,
    total_cost: asNumber(row.cost),
    plan_rows: asNumber(row.cardinality),
    actual_rows: perLoop(totalRows, starts),
    actual_time_ms: perLoop(totalMs, starts),
    actual_loops: starts,
    buffers_hit: crGets == null && cuGets == null ? null : (crGets ?? 0) + (cuGets ?? 0),
    buffers_read: statistics ? asNumber(row.disk_reads) : null,
    filter: asText(row.filter_predicates),
    index_condition: isIndexAccess ? access : null,
    join_type: isJoin(operation) ? (options ?? "INNER") : null,
    hash_condition: isHashLike ? access : null,
    extra,
    children: [],
  };
}

function parsePayload(payload: string): OraclePlanPayload {
  let value: unknown;
  try {
    value = JSON.parse(payload);
  } catch (error) {
    throw new Error(`Invalid Oracle plan payload: ${(error as Error).message}`);
  }
  if (typeof value !== "object" || value === null) {
    throw new Error("Invalid Oracle plan payload: expected a JSON object");
  }
  const object = value as Partial<OraclePlanPayload>;
  if (object.version !== SUPPORTED_VERSION) {
    throw new Error(
      `Unsupported Oracle plan payload version ${String(object.version)}; this parser reads version ${SUPPORTED_VERSION}`,
    );
  }
  if (!Array.isArray(object.plan)) {
    throw new Error("Invalid Oracle plan payload: missing 'plan' array");
  }
  for (const row of object.plan) {
    if (typeof row?.id !== "number" || typeof row?.operation !== "string") {
      throw new Error("Invalid Oracle plan payload: every row needs a numeric 'id' and an 'operation'");
    }
  }
  return object as OraclePlanPayload;
}

/**
 * Parse an `oracle-plan-json` payload (rows from `PLAN_TABLE`, or from
 * `V$SQL_PLAN_STATISTICS_ALL` for analyzed plans) into the shared plan model.
 */
export function parseOraclePlan(payload: string): ExplainPlan {
  const { plan: rows, statistics, text } = parsePayload(payload);
  if (rows.length === 0) {
    throw new Error("Oracle plan payload contains no plan operations");
  }

  const byId = new Map<number, { row: OraclePlanRow; node: ExplainNode }>();
  for (const row of rows) {
    byId.set(row.id, { row, node: toNode(row, statistics) });
  }

  const roots: { row: OraclePlanRow; node: ExplainNode }[] = [];
  const childrenOf = new Map<number, { row: OraclePlanRow; node: ExplainNode }[]>();
  for (const entry of byId.values()) {
    const parent = entry.row.parent_id;
    if (parent == null || !byId.has(parent)) {
      roots.push(entry);
    } else {
      const siblings = childrenOf.get(parent) ?? [];
      siblings.push(entry);
      childrenOf.set(parent, siblings);
    }
  }

  // Oracle orders siblings by POSITION; fall back to id for missing values.
  const order = (a: { row: OraclePlanRow }, b: { row: OraclePlanRow }) =>
    (asNumber(a.row.position) ?? a.row.id) - (asNumber(b.row.position) ?? b.row.id) ||
    a.row.id - b.row.id;
  for (const [parent, children] of childrenOf) {
    children.sort(order);
    byId.get(parent)!.node.children = children.map((child) => child.node);
  }
  roots.sort((a, b) => a.row.id - b.row.id);
  const root = roots[0]!.node;

  // The statement-level row often carries no timing of its own; its first
  // child then holds the whole execution.
  const rootTime = root.extra.actual_time_ms_total ?? root.children[0]?.extra.actual_time_ms_total;
  const hasAnalyzeData =
    statistics && rows.some((row) => asNumber(row.actual_rows) != null);

  return {
    root,
    planning_time_ms: null,
    execution_time_ms: hasAnalyzeData && typeof rootTime === "number" ? rootTime : null,
    original_query: "",
    driver: "oracle",
    has_analyze_data: hasAnalyzeData,
    raw_output: asText(text) ?? payload,
  };
}
