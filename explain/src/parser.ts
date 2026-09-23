import type { RegisteredExplainParser } from "@tabularis/explain";

import { parseOraclePlan } from "./plan";

/** Oracle parser descriptor consumed by both package and plugin loaders. */
export const oracleExplainParser: RegisteredExplainParser = {
  engine: "oracle",
  format: "oracle-plan-json",
  label: "Oracle execution plan (JSON)",
  parse: parseOraclePlan,
  // Cheap, order-independent check: a JSON object whose plan rows carry
  // Oracle's PLAN_TABLE column names. The parser still validates fully.
  sniff: (payload) => {
    const head = payload.slice(0, 4096);
    return (
      /^\s*\{/.test(head) &&
      /"plan"\s*:\s*\[/.test(head) &&
      head.includes('"qblock_name"')
    );
  },
};
