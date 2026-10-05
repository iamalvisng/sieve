export function double(n: number): number {
  return n * 2;
}

export function quadruple(n: number): number {
  return double(double(n));
}
