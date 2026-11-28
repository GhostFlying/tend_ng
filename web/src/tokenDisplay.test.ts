import { describe, expect, it } from "vitest";
import { compactTokens, displayUsd, tokens } from "./tokenDisplay";

describe("displayUsd", () => {
  it.each([
    ["0", "$0.0"],
    ["0.000000000000", "$0.0"],
    ["0.000000000001", "<$0.1"],
    ["0.049999999999", "<$0.1"],
    ["0.05", "$0.1"],
    ["1.0942952", "$1.1"],
    ["99.95", "$100.0"],
    ["9007199254740993.15", "$9,007,199,254,740,993.2"],
  ])("displays %s as %s", (exact, displayed) => {
    expect(displayUsd(exact)).toBe(displayed);
  });
});

describe("Token counts", () => {
  it("keeps exact counts without passing through Number", () => {
    const field = { tokens: "9007199254740993", reported_requests: 1 };
    expect(tokens(field)).toBe("9,007,199,254,740,993");
    expect(compactTokens(field)).toBe("9,007.2T");
  });

  it("distinguishes omitted counts from observed zero", () => {
    expect(compactTokens({ tokens: null, reported_requests: 0 })).toBe("—");
    expect(compactTokens({ tokens: "0", reported_requests: 1 })).toBe("0");
  });
});
