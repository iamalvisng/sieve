export function parseHeader(line: string): string {
  return line.split(":")[0];
}

export function configName(): string {
  return "alpha";
}
