export function betaGreet(name: string): string {
  return `hi ${name}`;
}

export function betaShout(name: string): string {
  return betaGreet(name).toUpperCase();
}
