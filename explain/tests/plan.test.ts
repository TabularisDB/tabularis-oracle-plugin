import { readFile } from "node:fs/promises";

import type { ExplainNode, ExplainPlan } from "@tabularis/explain";
import { describe, expect, it } from "vitest";

import { parseOraclePlan } from "../src/plan";

const fixtureNames = [
  "hash-join-estimated",
  "hash-join-analyzed",
  "index-unique-scan",
  "nested-loops-analyzed",
  "json-column-analyzed",
  "view-sort-estimated",
] as const;
const fixtureDirectory = new URL("./fixtures/", import.meta.url);

/** Fixtures are `explain_query` payloads captured from Oracle Database 23ai Free. */
async function readPayload(name: string): Promise<string> {
  const fixture = JSON.parse(
    await readFile(new URL(`${name}.json`, fixtureDirectory), "utf8"),
  ) as { payload: string };
  return fixture.payload;
}

function flatten(node: ExplainNode): ExplainNode[] {
  return [node, ...node.children.flatMap(flatten)];
}

describe("parseOraclePlan", () => {
  it.each(fixtureNames)("matches the committed golden plan for %s", async (name) => {
    const expected = JSON.parse(
      await readFile(new URL(`expected/${name}.json`, fixtureDirectory), "utf8"),
    ) as ExplainPlan;

    expect(parseOraclePlan(await readPayload(name))).toEqual(expected);
  });

  it("builds the tree from PARENT_ID and maps estimates", async () => {
    const plan = parseOraclePlan(await readPayload("hash-join-estimated"));

    expect(plan.root).toMatchObject({ id: "oracle-0", node_type: "SELECT STATEMENT" });
    expect(flatten(plan.root).map((node) => node.id)).toEqual([
      "oracle-0",
      "oracle-1",
      "oracle-2",
      "oracle-3",
      "oracle-4",
      "oracle-5",
      "oracle-6",
    ]);
    const hashJoin = flatten(plan.root).find((node) => node.id === "oracle-3");
    expect(hashJoin).toMatchObject({
      node_type: "HASH JOIN",
      join_type: "INNER",
      hash_condition: '"O"."CUSTOMER_ID"="C"."ID"',
      total_cost: 6,
      plan_rows: 4,
    });
    const scan = flatten(plan.root).find((node) => node.id === "oracle-4");
    expect(scan).toMatchObject({
      node_type: "TABLE ACCESS FULL",
      relation: "CUSTOMERS",
      filter: `"C"."REGION"='West'`,
      actual_rows: null,
    });
    expect(plan.has_analyze_data).toBe(false);
    expect(plan.execution_time_ms).toBeNull();
    expect(plan.raw_output).toContain("Plan hash value");
  });

  it("maps index access predicates to index_condition", async () => {
    const plan = parseOraclePlan(await readPayload("index-unique-scan"));
    const index = flatten(plan.root).find((node) => node.node_type === "INDEX UNIQUE SCAN");

    expect(index?.index_condition).toBe('"ID"=3');
    expect(index?.extra.object_type).toBe("INDEX (UNIQUE)");
  });

  it("converts runtime totals to per-loop values", async () => {
    const plan = parseOraclePlan(await readPayload("nested-loops-analyzed"));
    const nodes = flatten(plan.root);

    expect(plan.has_analyze_data).toBe(true);
    expect(plan.execution_time_ms).toBeGreaterThan(0);
    for (const node of nodes) {
      const total = node.extra.actual_rows_total as number | undefined;
      if (total !== undefined && node.actual_loops) {
        expect(node.actual_rows! * node.actual_loops).toBeCloseTo(total);
      }
    }
    const rangeScan = nodes.find((node) => node.node_type === "INDEX RANGE SCAN");
    expect(rangeScan).toMatchObject({
      relation: "ORDERS_CUSTOMER_IX",
      index_condition: '"O"."CUSTOMER_ID"="C"."ID"',
    });
    expect(rangeScan?.actual_loops).toBeGreaterThan(1);
  });

  it("rejects malformed and unsupported payloads", () => {
    expect(() => parseOraclePlan("not json")).toThrow(/Invalid Oracle plan payload/);
    expect(() => parseOraclePlan('{"version":2,"plan":[]}')).toThrow(/version 2/);
    expect(() => parseOraclePlan('{"version":1,"statistics":false,"plan":[]}')).toThrow(
      /no plan operations/,
    );
    expect(() =>
      parseOraclePlan('{"version":1,"statistics":false,"plan":[{"id":"x"}]}'),
    ).toThrow(/numeric 'id'/);
  });
});


describe("DBMS_XPLAN text", () => {
  it("parses captured Oracle plans with their hierarchy and runtime rows", async () => {
    const { parseOracleText } = await import("../src/text");
    const fixture = JSON.parse(await readFile(new URL("./fixtures/hash-join-analyzed.json", import.meta.url), "utf8"));
    const payload = JSON.parse(fixture.payload);
    const plan = parseOracleText(payload.text);
    expect(plan.root.node_type).toBe("SELECT STATEMENT");
    expect(plan.root.children[0].node_type).toBe("SORT GROUP BY NOSORT");
    expect(plan.root.actual_rows).toBe(1);
    expect(plan.has_analyze_data).toBe(true);
  });
});


describe("captured DBMS_XPLAN report compatibility", () => {
  it.each(["hash-join-analyzed", "hash-join-estimated", "index-unique-scan", "json-column-analyzed", "nested-loops-analyzed", "view-sort-estimated"])("preserves operation hierarchy in %s", async (name) => {
    const { parseOracleText } = await import("../src/text");
    const fixture = JSON.parse(await readFile(new URL(`./fixtures/${name}.json`, import.meta.url), "utf8"));
    const payload = JSON.parse(fixture.payload);
    const expected = parseOraclePlan(fixture.payload);
    const actual = parseOracleText(payload.text);
    const shape = (node: typeof actual.root): unknown => [node.node_type, node.relation, node.children.map(shape)];
    expect(shape(actual.root)).toEqual(shape(expected.root));
  });
});


it("rejects truncated DBMS_XPLAN tables", async () => {
  const { parseOracleText } = await import("../src/text");
  const fixture = JSON.parse(await readFile(new URL("./fixtures/hash-join-analyzed.json", import.meta.url), "utf8"));
  const text = JSON.parse(fixture.payload).text as string;
  const lines = text.split("\n");
  const operation = lines.findIndex((line) => /\|\s*0\s*\|/.test(line));
  expect(() => parseOracleText(lines.slice(0, operation + 1).join("\n"))).toThrow(/incomplete/);
});
