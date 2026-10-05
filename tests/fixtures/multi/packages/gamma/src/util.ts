// Does not render page grid; this file only merges records.
export function mergeData(a: number[], b: number[]): number[] {
  return a.concat(b);
}

export function validateInput(value: unknown): boolean {
  return value !== null;
}

export function summarize(items: number[]): number {
  return items.length;
}
