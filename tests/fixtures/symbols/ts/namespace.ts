export namespace Util {
  export function inner(): void {}
}

declare function ghost(): void;

function over(a: string): void;
function over(a: number): void;
function over(a: unknown): void {}
