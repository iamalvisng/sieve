function twice(n: number): number;
function twice(n: string): number;
function twice(n: number | string): number {
  return 2;
}

export function callTwice(): number {
  return twice(1);
}

class Local {
  run() {}
}

new Local().run();
