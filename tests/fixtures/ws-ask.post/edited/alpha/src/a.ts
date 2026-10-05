export function parseConfig(text: string): string {
  return text.trim();
}

export function loadConfig(path: string): string {
  return parseConfig(path);
}

export function parseConfigFile(path: string): string {
  return loadConfig(path);
}

export function parseConfigExtra(): number {
  return 3;
}
