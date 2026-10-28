import { greet } from "./def";

export function inner(): string {
  const f = (greet: string) => greet + "!";
  {
    let greet = 1;
    void greet;
  }
  return f("x");
}

export const top = greet("c");
