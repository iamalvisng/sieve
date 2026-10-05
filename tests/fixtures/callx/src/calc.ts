import { Widget } from "./base";

export function double(n: number): number {
  return n * 2;
}

export function doubled(n: number): number {
  return n + n;
}

export function zed(n: number): number {
  const a = doubled(n);
  const w: Widget = new Widget();
  return double(a) + w.size();
}

export function alpha(n: number): number {
  return double(n);
}
