import type { TokenField } from "./api";

export const format = new Intl.NumberFormat("zh-CN");

export function tokens(field: TokenField) {
  return field.tokens === null ? "—" : format.format(BigInt(field.tokens));
}

export function compactTokens(field: TokenField) {
  if (field.tokens === null) return "—";
  const value = BigInt(field.tokens);
  for (const [unit, size] of [
    ["T", 10n ** 12n],
    ["B", 10n ** 9n],
    ["M", 10n ** 6n],
  ] as const) {
    if (value < size) continue;
    const tenths = (value * 10n + size / 2n) / size;
    const digit = tenths % 10n;
    return `${format.format(tenths / 10n)}${digit ? `.${digit}` : ""}${unit}`;
  }
  return tokens(field);
}

// Estimates are fixed-point decimal strings. Don't convert the ledger value
// to a float before rounding or infer a missing amount from its display.
export function displayUsd(usd: string) {
  const [whole, fraction = ""] = usd.split(".");
  const tenths =
    BigInt(whole) * 10n +
    BigInt(fraction[0] ?? "0") +
    (Number(fraction[1] ?? "0") >= 5 ? 1n : 0n);
  return tenths === 0n && /[1-9]/.test(usd)
    ? "<$0.1"
    : `$${format.format(tenths / 10n)}.${tenths % 10n}`;
}
