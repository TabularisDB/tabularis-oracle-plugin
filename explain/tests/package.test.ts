import { readFile } from "node:fs/promises";

import {
  registrations,
  type RegisteredExplainParser,
} from "@tabularis/explain";
import { describe, expect, it } from "vitest";

import { oracleExplainParser, parseOraclePlan } from "../src/index";

async function fixturePayload(): Promise<string> {
  const fixture = JSON.parse(
    await readFile(new URL("./fixtures/hash-join-estimated.json", import.meta.url), "utf8"),
  ) as { payload: string };
  return fixture.payload;
}

describe("package entry points", () => {
  it("registers on ESM import and exports the direct parser API", async () => {
    expect(registrations).toEqual([oracleExplainParser]);
    expect(oracleExplainParser).toMatchObject({
      engine: "oracle",
      format: "oracle-plan-json",
      label: "Oracle execution plan (JSON)",
      parse: parseOraclePlan,
    });
    expect(oracleExplainParser.sniff?.(await fixturePayload())).toBe(true);
    expect(oracleExplainParser.sniff?.('{"Plan": {"Node Type": "Seq Scan"}}')).toBe(false);
    expect(oracleExplainParser.sniff?.("<ShowPlanXML/>")).toBe(false);
  });

  it("builds an isolated IIFE descriptor without self-registration", async () => {
    const source = await readFile(new URL("../dist/index.iife.js", import.meta.url), "utf8");
    const registrationsBeforeEvaluation = registrations.length;
    const evaluate = new Function(
      "__TABULARIS_EXPLAIN__",
      `${source}\nreturn typeof __tabularis_explain_parser__ !== "undefined" ? __tabularis_explain_parser__ : null;`,
    );
    const raw = evaluate({}) as Record<string, unknown>;
    const descriptor = (raw.default ?? raw) as RegisteredExplainParser;

    expect(descriptor).toMatchObject({
      engine: "oracle",
      format: "oracle-plan-json",
      label: "Oracle execution plan (JSON)",
    });
    expect(descriptor.parse(await fixturePayload()).driver).toBe("oracle");
    expect(registrations).toHaveLength(registrationsBeforeEvaluation);
  });
});
