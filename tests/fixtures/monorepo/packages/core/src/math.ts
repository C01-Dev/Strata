/** Sum of all values. */
export function sum(values: readonly number[]): number {
  return values.reduce((total, value) => total + value, 0);
}

/** Arithmetic mean; `undefined` for an empty list. */
export function mean(values: readonly number[]): number | undefined {
  return values.length ? sum(values) / values.length : undefined;
}

export const GOLDEN_RATIO = (1 + Math.sqrt(5)) / 2;
