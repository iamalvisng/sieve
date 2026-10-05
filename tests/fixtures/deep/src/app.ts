import { quadruple } from "./util";
import { z } from "zod";

export class App {
  name: string;

  constructor(name: string) {
    this.name = name;
  }

  run(n: number): number {
    return quadruple(n);
  }
}

export function main(): void {
  const app = new App("sieve");
  app.run(1);
  z.string();
}
