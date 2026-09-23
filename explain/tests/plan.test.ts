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
