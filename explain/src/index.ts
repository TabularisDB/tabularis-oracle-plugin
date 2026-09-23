import { registerExplainParser } from "@tabularis/explain";

import { oracleExplainParser } from "./parser";

registerExplainParser(oracleExplainParser);

export { oracleExplainParser } from "./parser";
export { parseOraclePlan } from "./plan";
export type { OraclePlanPayload, OraclePlanRow } from "./plan";
